//! Files' verbs by a path in the workspace, carried out where the workspace's tree is kept:
//! `files#new-folder`, `#move`, `#rename`, `#copy-into`, `#trash` and `#restore`; and its reads
//! by the same path, `files#list` and `#read-text`, answered there with no window.
//!
//! A workspace's folder is on no one disk, so these name a path in it and no device. A device's
//! agent asks them of the workspace through its session. With no page open and no device of
//! the person connected at all, the server's own action service answers them the same, which
//! is how an agent that works while its person's devices are off keeps a folder for each task.
//! An agent on a turn the server carries out asks them through that turn's tools, held to the
//! rules the turn carries: one its rules refuse Files is refused the folder.

use super::super::hosted_agent_turns::{SCOUT, make_agent};
use super::super::hosted_documents::ask;
use super::*;
use serde_json::Value;
use tilo_actions::remote::{
    ActionProvider as _, Caller, ContentPutAnswer, ContentPutStep, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate, api::ActionAddress};
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::agents::WorkspaceActionProvider;

/// The folder the work on one task keeps its material in.
const TASK: &str = "Tasks/12 Plan the launch";

/// What the notes in the task's folder say.
const NOTES: &str = "Launch on Friday.\nBook the hall.\n";

/// The first bytes of a picture: no text.
const PHOTO: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];

/// Whether `dir/name` is a folder of `tree`.
fn folder_at(tree: &Tree, dir: &str, name: &str) -> bool {
    file_at(tree, dir, name).is_some_and(|node| node.kind == FileNodeKind::Directory)
}

/// What the file at `dir/name` of `tree` names its content by.
fn content_at(tree: &Tree, dir: &str, name: &str) -> Option<String> {
    file_at(tree, dir, name)
        .filter(|node| node.kind == FileNodeKind::File)
        .and_then(|node| node.hash.clone())
}

/// One of Files' verbs asked through the agent's session: what it answered as text.
async fn files(server: &McpServer<SessionActions>, verb: &str, arguments: Value) -> String {
    tool(
        server,
        "tilo_do",
        json!({"path": format!("files#{verb}"), "args": arguments}),
    )
    .await
}

/// One of Files' verbs that must be refused: the words it is refused with.
async fn files_refused(server: &McpServer<SessionActions>, verb: &str, arguments: Value) -> String {
    let answer = ask(
        server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "tilo_do",
                   "arguments": {"path": format!("files#{verb}"), "args": arguments}}}),
    )
    .await;
    let result = &answer["result"];
    assert!(
        result["isError"] == true || answer.get("error").is_some(),
        "files#{verb} was not refused: {answer}"
    );
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        answer.to_string()
    } else {
        text
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_devices_agent_keeps_a_folder_for_a_task_by_paths_in_the_workspace() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "filing agent", DeviceCapabilities::member());
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
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    );

    // Two files arrive in the workspace's folder.
    let visa = form_pdf("Application for Schengen Visa");
    let permit = form_pdf("Application for Residence Permit");
    for (name, bytes) in [("visa.pdf", &visa), ("permit.pdf", &permit)] {
        let content = content_in(&put(&server, bytes).await);
        files(&server, "open", json!({"content": content, "name": name})).await;
    }

    let initial = tree_when(&hosted, WORKSPACE, |tree| {
        content_at(tree, "", "visa.pdf").is_some() && content_at(tree, "", "permit.pdf").is_some()
    })
    .await;
    let visa_hash = content_at(&initial, "", "visa.pdf").unwrap();
    let permit_hash = content_at(&initial, "", "permit.pdf").unwrap();
    assert_sealed_file(&hosted.server, WORKSPACE, &visa_hash, &visa).await;
    assert_sealed_file(&hosted.server, WORKSPACE, &permit_hash, &permit).await;

    // The task gets its folder, the folder on the way is made with it, and both files move
    // into it in one call.
    files(
        &server,
        "new-folder",
        json!({"parent": "Tasks", "name": "12 Plan the launch"}),
    )
    .await;
    let moved = files(
        &server,
        "move",
        json!({"from": "", "entries": ["visa.pdf", "permit.pdf"], "to": TASK}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        content_at(tree, TASK, "visa.pdf").is_some()
            && content_at(tree, TASK, "permit.pdf").is_some()
    })
    .await;
    assert!(folder_at(&tree, "Tasks", "12 Plan the launch"), "{moved}");
    assert_eq!(
        content_at(&tree, TASK, "visa.pdf"),
        Some(visa_hash.clone()),
        "the file is the same file in its new folder: {moved}"
    );
    assert_eq!(
        content_at(&tree, TASK, "permit.pdf"),
        Some(permit_hash.clone())
    );
    assert!(
        file_at(&tree, "", "visa.pdf").is_none() && file_at(&tree, "", "permit.pdf").is_none(),
        "moved, not copied: {moved}"
    );

    // A rename in place, and a copy beside what it copies, which keeps both.
    files(
        &server,
        "rename",
        json!({"folder": TASK, "entry": "visa.pdf", "name": "Visa form.pdf"}),
    )
    .await;
    let copied = files(
        &server,
        "copy-into",
        json!({"from": TASK, "entries": ["permit.pdf"], "to": TASK}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        content_at(tree, TASK, "Visa form.pdf").is_some()
            && content_at(tree, TASK, "permit (2).pdf").is_some()
    })
    .await;
    assert_eq!(
        content_at(&tree, TASK, "Visa form.pdf"),
        Some(visa_hash.clone())
    );
    assert!(file_at(&tree, TASK, "visa.pdf").is_none());
    assert_eq!(
        content_at(&tree, TASK, "permit (2).pdf"),
        Some(permit_hash.clone()),
        "{copied}"
    );
    assert_eq!(
        content_at(&tree, TASK, "permit.pdf"),
        Some(permit_hash.clone()),
        "what was copied is still there"
    );

    // The trash takes the copy, and gives it back to where it was.
    files(
        &server,
        "trash",
        json!({"folder": TASK, "entries": ["permit (2).pdf"]}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, TASK, "permit (2).pdf").is_none()
    })
    .await;
    assert!(file_at(&tree, TASK, "permit (2).pdf").is_none());
    assert!(file_at(&tree, TASK, "permit.pdf").is_some());
    files(&server, "restore", json!({"entries": ["permit (2).pdf"]})).await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, TASK, "permit (2).pdf").is_some()
    })
    .await;
    assert_eq!(
        content_at(&tree, TASK, "permit (2).pdf"),
        Some(permit_hash.clone())
    );

    // The folder is read by its path, with no window: what it holds, and what a file in it
    // says. A file that is no text is refused with what reads it.
    for (name, bytes) in [("notes.md", NOTES.as_bytes()), ("photo.jpg", PHOTO)] {
        let content = content_in(&put(&server, bytes).await);
        files(
            &server,
            "open",
            json!({"content": content, "name": name, "dir": TASK}),
        )
        .await;
    }
    tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, TASK, "notes.md").is_some() && file_at(tree, TASK, "photo.jpg").is_some()
    })
    .await;
    let listed = files(&server, "list", json!({"folder": TASK})).await;
    for name in [
        "Visa form.pdf",
        "permit.pdf",
        "permit (2).pdf",
        "notes.md",
        "photo.jpg",
    ] {
        assert!(listed.contains(name), "{name} is not listed: {listed}");
    }
    // A file's name is whoever named it's words, as what it says is whoever wrote it's: both
    // reach a device's agent labelled as content from outside.
    assert!(listed.contains(outside::OUTSIDE), "{listed}");
    let read = files(
        &server,
        "read-text",
        json!({"file": format!("{TASK}/notes.md")}),
    )
    .await;
    assert!(read.contains("Book the hall."), "{read}");
    assert!(read.contains(outside::OUTSIDE), "{read}");
    let said = files_refused(
        &server,
        "read-text",
        json!({"file": format!("{TASK}/photo.jpg")}),
    )
    .await;
    assert!(said.contains("is not text"), "{said}");
    let said = files_refused(&server, "list", json!({"folder": "../elsewhere"})).await;
    assert!(said.contains("no name in the workspace"), "{said}");

    // A path never leaves the workspace's folder, and a verb writes over nothing.
    let said = files_refused(
        &server,
        "move",
        json!({"from": TASK, "entries": ["permit.pdf"], "to": "../elsewhere"}),
    )
    .await;
    assert!(said.contains("no name in the workspace"), "{said}");
    let said = files_refused(
        &server,
        "new-folder",
        json!({"parent": "/home/ada", "name": "x"}),
    )
    .await;
    assert!(said.contains("no leading /"), "{said}");
    let said = files_refused(
        &server,
        "rename",
        json!({"folder": TASK, "entry": "permit.pdf", "name": "Visa form.pdf"}),
    )
    .await;
    assert!(said.contains("is taken"), "{said}");
    let tree = tree_of(&hosted, WORKSPACE).await;
    assert_eq!(
        content_at(&tree, TASK, "permit.pdf"),
        Some(permit_hash.clone()),
        "a refused verb changed nothing"
    );
    assert_eq!(
        content_at(&tree, TASK, "Visa form.pdf"),
        Some(visa_hash.clone())
    );
    assert!(
        !folder_at(&tree, "", "elsewhere") && !folder_at(&tree, "", "home"),
        "no folder was made for a refused path"
    );
}

/// Ask the server's own action service for Files' `verb` on the folder of `workspace`, as
/// `caller`: what it answered, or the words it refused with.
pub(super) async fn asked(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    verb: &str,
    arguments: Value,
) -> Result<Value, String> {
    let target = RemoteActionTarget {
        provider: tilo_actions::remote::workspace_provider(workspace),
        address: ActionAddress {
            resource: format!("app.{FILES_APP}"),
            action: verb.to_owned(),
        },
    };
    asked_of(provider, caller, target, arguments).await
}

/// Invoke `target` on the server's own action service as `caller`: what it answered, or the
/// words it refused with.
pub(super) async fn asked_of(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    target: RemoteActionTarget,
    arguments: Value,
) -> Result<Value, String> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let answered = |output| {
        serde_json::to_value(output)
            .ok()
            .and_then(|output: Value| output.get("value").cloned())
            .unwrap_or(Value::Null)
    };
    let (provider, caller) = (provider.clone(), caller.clone());
    tokio::task::spawn_blocking(move || {
        let invocation = request.invocation;
        let receipt = provider
            .invoke(&caller, request)
            .wait_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| "the server did not answer in time".to_owned())?
            .ok_or("the server dropped the call")?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(answered(output)),
            ReceiptStatus::Accepted => {
                let started = std::time::Instant::now();
                loop {
                    match provider
                        .observe(&caller, &invocation)
                        .map(|state| state.update)
                    {
                        Some(InvocationUpdate::Succeeded { output, .. }) => {
                            return Ok(answered(output));
                        }
                        Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
                        Some(InvocationUpdate::Cancelled { .. }) => {
                            return Err("the call was cancelled".into());
                        }
                        _ => {}
                    }
                    if started.elapsed() > std::time::Duration::from_secs(30) {
                        return Err("the call did not end in time".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            ReceiptStatus::Failed { error } => Err(error.message),
            ReceiptStatus::Rejected { error } => Err(error.to_string()),
            ReceiptStatus::Cancelled { .. } => Err("the call was cancelled".into()),
        }
    })
    .await
    .unwrap()
}

/// The tree `workspaces` keeps of `workspace` once it reads as `ready` says, or as it is after
/// ten seconds.
async fn kept_when(
    workspaces: &Workspaces,
    workspace: &str,
    ready: impl Fn(&Tree) -> bool,
) -> Tree {
    let started = std::time::Instant::now();
    loop {
        let tree = tree_kept(workspaces, workspace).await;
        if ready(&tree) || started.elapsed() > std::time::Duration::from_secs(10) {
            return tree;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_page_open_the_server_keeps_a_folder_for_a_task_by_paths() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server alone: no page is opened, and no device of the person ever joins.
    let (server, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let provider = WorkspaceActionProvider::new(
        actions.clone(),
        tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap(),
    );
    // The person, on the server that works for them.
    let person =
        Caller::authenticated(root.verifying_key().to_bytes(), host.cert.device_public_key)
            .in_workspace(&workspace);

    // The task's folder, with the folder on the way; asked again it is the same folder.
    let made = asked(
        &provider,
        &person,
        &workspace,
        "new-folder",
        json!({"parent": "Tasks", "name": "12 Plan the launch"}),
    )
    .await
    .unwrap();
    assert_eq!(made["entries"][0]["to"], TASK, "{made}");
    assert_eq!(made["made"], json!(["Tasks"]), "{made}");
    assert!(made["invocation"].is_string(), "the call it was: {made}");
    let again = asked(
        &provider,
        &person,
        &workspace,
        "new-folder",
        json!({"parent": "Tasks", "name": "12 Plan the launch"}),
    )
    .await
    .unwrap();
    assert_eq!(again["entries"], json!([]), "{again}");

    // Material gathered elsewhere moves into it in one call, which answers the move back.
    for name in ["drafts", "sources"] {
        asked(
            &provider,
            &person,
            &workspace,
            "new-folder",
            json!({"parent": "Inbox", "name": name}),
        )
        .await
        .unwrap();
    }
    let moved = asked(
        &provider,
        &person,
        &workspace,
        "move",
        json!({"from": "Inbox", "entries": ["drafts", "sources"], "to": TASK}),
    )
    .await
    .unwrap();
    assert!(moved["invocation"].is_string(), "{moved}");
    let renamed = asked(
        &provider,
        &person,
        &workspace,
        "rename",
        json!({"folder": TASK, "entry": "drafts", "name": "Drafts"}),
    )
    .await
    .unwrap();
    assert_eq!(
        renamed["entries"][0]["to"],
        format!("{TASK}/Drafts"),
        "{renamed}"
    );
    asked(
        &provider,
        &person,
        &workspace,
        "copy-into",
        json!({"from": "Tasks", "entries": ["12 Plan the launch"], "to": "Archive/2026"}),
    )
    .await
    .unwrap();

    // The trash takes a folder, and History's undo of that call brings it back.
    let gone = asked(
        &provider,
        &person,
        &workspace,
        "trash",
        json!({"folder": TASK, "entries": ["sources"]}),
    )
    .await
    .unwrap();
    let tree = kept_when(&workspaces, &workspace, |tree| {
        !folder_at(tree, TASK, "sources")
    })
    .await;
    assert!(!folder_at(&tree, TASK, "sources"), "{gone}");
    let back = asked(
        &provider,
        &person,
        &workspace,
        "undo",
        json!({"invocation": gone["invocation"]}),
    )
    .await
    .unwrap();
    assert_eq!(back["taken"], 1, "{back}");

    // What the server keeps is the tree every member's folder shows.
    let tree = kept_when(&workspaces, &workspace, |tree| {
        folder_at(tree, TASK, "sources")
    })
    .await;
    assert!(folder_at(&tree, "Tasks", "12 Plan the launch"));
    assert!(folder_at(&tree, TASK, "Drafts") && folder_at(&tree, TASK, "sources"));
    assert!(!folder_at(&tree, TASK, "drafts"));
    assert!(
        !folder_at(&tree, "Inbox", "drafts") && !folder_at(&tree, "Inbox", "sources"),
        "moved out of where they were"
    );
    assert!(
        folder_at(&tree, "Archive/2026/12 Plan the launch", "Drafts"),
        "a folder is copied with all it holds"
    );

    // A device is never named here, and a path never leaves the workspace's folder.
    let said = asked(
        &provider,
        &person,
        &workspace,
        "new-folder",
        json!({"device": "laptop", "parent": "/home/ada", "name": "x"}),
    )
    .await
    .unwrap_err();
    assert!(said.contains("names no device"), "{said}");
    let said = asked(
        &provider,
        &person,
        &workspace,
        "move",
        json!({"from": "Tasks", "entries": ["12 Plan the launch"], "to": "../outside"}),
    )
    .await
    .unwrap_err();
    assert!(said.contains("no name in the workspace"), "{said}");
    let said = asked(
        &provider,
        &person,
        &workspace,
        "trash",
        json!({"folder": "/etc", "entries": ["passwd"]}),
    )
    .await
    .unwrap_err();
    assert!(said.contains("no leading /"), "{said}");
    let tree = tree_kept(&workspaces, &workspace).await;
    assert!(
        folder_at(&tree, "Tasks", "12 Plan the launch") && !folder_at(&tree, "", "outside"),
        "a refused verb changed nothing"
    );
}

/// `bytes` put into the store of `workspace` through the server's own action service, as
/// `caller`: the content's reference.
pub(super) async fn uploaded(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: tilo_server_protocol::WorkspaceId,
    caller: &Caller,
    bytes: &[u8],
) -> String {
    let (actions, caller, bytes) = (actions.clone(), caller.clone(), bytes.to_vec());
    tokio::task::spawn_blocking(move || {
        let step = |step| {
            let answer = actions
                .serve(
                    workspace,
                    &caller,
                    RemoteActionMessage::ContentPutRequest {
                        correlation: 1,
                        step,
                    },
                )
                .unwrap()
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .unwrap();
            match answer {
                RemoteActionMessage::ContentPutResponse { result, .. } => result.unwrap(),
                other => panic!("expected an upload's answer, got {other:?}"),
            }
        };
        let ContentPutAnswer::Begun { upload } = step(ContentPutStep::Begin {
            size: bytes.len() as u64,
        }) else {
            panic!("an upload begins")
        };
        step(ContentPutStep::append(upload, 0, &bytes));
        match step(ContentPutStep::Finish { upload }) {
            ContentPutAnswer::Finished { content } => content,
            other => panic!("expected the content, got {other:?}"),
        }
    })
    .await
    .unwrap()
}

/// The names a `list` answered, in its order.
fn names_in(listed: &Value) -> Vec<&str> {
    listed["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["name"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_page_open_a_folder_is_listed_and_a_file_read_back_by_its_path() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server alone: no page is opened, and no device of the person ever joins.
    let (server, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    let provider = WorkspaceActionProvider::new(actions.clone(), id);
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let list = |arguments: Value| asked(&provider, &person, &workspace, "list", arguments);
    let read = |arguments: Value| asked(&provider, &person, &workspace, "read-text", arguments);

    // A workspace nobody put anything into lists as empty.
    let empty = list(json!({})).await.unwrap();
    assert_eq!(empty["total"], 0, "{empty}");
    assert_eq!(empty["entries"], json!([]), "{empty}");

    // A note and a picture are put into the task's folder, beside two folders.
    for (name, bytes) in [("notes.md", NOTES.as_bytes()), ("photo.jpg", PHOTO)] {
        let content = uploaded(&actions, id, &person, bytes).await;
        let put = asked(
            &provider,
            &person,
            &workspace,
            "open",
            json!({"content": content, "name": name, "dir": TASK}),
        )
        .await
        .unwrap();
        assert_eq!(put["path"], format!("{TASK}/{name}"), "{put}");
    }
    for name in ["sources", "Drafts"] {
        asked(
            &provider,
            &person,
            &workspace,
            "new-folder",
            json!({"parent": TASK, "name": name}),
        )
        .await
        .unwrap();
    }

    // The folder is listed by its path: names in order, kinds, a file's size, when it changed.
    let listed = list(json!({"folder": TASK})).await.unwrap();
    assert_eq!(
        names_in(&listed),
        ["Drafts", "notes.md", "photo.jpg", "sources"],
        "{listed}"
    );
    assert_eq!(listed["folder"], TASK, "{listed}");
    assert_eq!(listed["total"], 4, "{listed}");
    assert_eq!(listed["next"], Value::Null, "{listed}");
    let folder = &listed["entries"][0];
    assert_eq!(folder["kind"], "folder", "{listed}");
    assert_eq!(folder["size"], Value::Null, "{listed}");
    assert_eq!(folder["path"], format!("{TASK}/Drafts"), "{listed}");
    let note = &listed["entries"][1];
    assert_eq!(note["kind"], "file", "{listed}");
    assert_eq!(note["path"], format!("{TASK}/notes.md"), "{listed}");
    assert_eq!(note["size"], NOTES.len(), "{listed}");
    assert!(
        note["changed"].as_u64().is_some_and(|changed| changed > 0),
        "{listed}"
    );
    // A page at a time: the first says where the next begins, the last that none follows.
    let first = list(json!({"folder": TASK, "limit": 3})).await.unwrap();
    assert_eq!(
        names_in(&first),
        ["Drafts", "notes.md", "photo.jpg"],
        "{first}"
    );
    assert_eq!(first["next"], 3, "{first}");
    assert_eq!(first["total"], 4, "{first}");
    let rest = list(json!({"folder": TASK, "from": first["next"], "limit": 3}))
        .await
        .unwrap();
    assert_eq!(names_in(&rest), ["sources"], "{rest}");
    assert_eq!(rest["next"], Value::Null, "{rest}");
    // The workspace's own folder is listed when none is named.
    let top = list(json!({})).await.unwrap();
    assert_eq!(names_in(&top), ["Tasks"], "{top}");

    // The note is read back by the same path, whole and from a place in it.
    let file = format!("{TASK}/notes.md");
    let whole = read(json!({"file": file})).await.unwrap();
    assert_eq!(whole["text"], NOTES, "{whole}");
    assert_eq!(whole["file"], file, "{whole}");
    assert_eq!(whole["total"], NOTES.chars().count(), "{whole}");
    assert_eq!(whole["next"], Value::Null, "{whole}");
    assert_eq!(whole["size"], NOTES.len(), "{whole}");
    assert_eq!(whole["id"], note["id"], "the node the folder listed");
    let tail = read(json!({"file": file, "from": 7})).await.unwrap();
    assert_eq!(tail["text"], "on Friday.\nBook the hall.\n", "{tail}");
    assert_eq!(tail["from"], 7, "{tail}");
    // A picture is no text, and is refused in words.
    let said = read(json!({"file": format!("{TASK}/photo.jpg")}))
        .await
        .unwrap_err();
    assert!(said.contains("is not text"), "{said}");

    // What the trash holds is listed with the id that brings it back.
    let gone = asked(
        &provider,
        &person,
        &workspace,
        "trash",
        json!({"folder": TASK, "entries": ["Drafts"]}),
    )
    .await
    .unwrap();
    let trash = list(json!({"trash": true})).await.unwrap();
    assert_eq!(trash["total"], 1, "{trash}");
    assert_eq!(
        trash["entries"][0]["id"], gone["entries"][0]["id"],
        "{trash}"
    );
    assert_eq!(
        trash["entries"][0]["was"],
        format!("{TASK}/Drafts"),
        "{trash}"
    );
    assert_eq!(trash["entries"][0]["kind"], "folder", "{trash}");
    let listed = list(json!({"folder": TASK})).await.unwrap();
    assert_eq!(
        names_in(&listed),
        ["notes.md", "photo.jpg", "sources"],
        "{listed}"
    );

    // A path never leaves the workspace's folder, a folder is no file, and no device is named.
    for (outside, why) in [
        ("../outside", "no name in the workspace"),
        ("/etc", "no leading /"),
        ("Tasks/../..", "no name in the workspace"),
    ] {
        let said = list(json!({"folder": outside})).await.unwrap_err();
        assert!(said.contains(why), "{outside}: {said}");
    }
    for (outside, why) in [
        ("/etc/passwd", "no leading /"),
        ("Tasks/../../etc/passwd", "no name in the workspace"),
        (TASK, "is a folder"),
        ("Tasks/12 Plan the launch/never.md", "has no"),
    ] {
        let said = read(json!({"file": outside})).await.unwrap_err();
        assert!(said.contains(why), "{outside}: {said}");
    }
    let said = list(json!({"folder": file})).await.unwrap_err();
    assert!(said.contains("is a file, not a folder"), "{said}");
    let said = list(json!({"device": "laptop", "folder": "/home/ada"}))
        .await
        .unwrap_err();
    assert!(said.contains("names no device"), "{said}");
    let said = read(json!({})).await.unwrap_err();
    assert!(said.contains("names the file"), "{said}");
}

/// Call `name` with `arguments` on the tools of a turn the server carries out: the text it
/// answered, and whether it failed.
async fn on_turn(
    tools: &McpServer<WorkspaceActionProvider>,
    name: &str,
    arguments: Value,
) -> (String, bool) {
    let answer = tokio::time::timeout(
        Duration::from_secs(30),
        tools.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })),
    )
    .await
    .unwrap_or_else(|_| panic!("{name} answers in time"))
    .unwrap_or_else(|| panic!("{name} answers"));
    let result = answer.get("result").cloned().unwrap_or(answer);
    let failed = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let text = result.get("content").and_then(Value::as_array).map_or_else(
        || result.to_string(),
        |content| {
            content
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        },
    );
    (text, failed)
}

/// One of Files' verbs or reads asked through a turn's tools: what it answered, which must
/// not be a failure.
async fn done(tools: &McpServer<WorkspaceActionProvider>, verb: &str, arguments: Value) -> String {
    let (text, failed) = on_turn(
        tools,
        "tilo_do",
        json!({"path": format!("files#{verb}"), "args": arguments}),
    )
    .await;
    assert!(!failed, "files#{verb} failed: {text}");
    text
}

/// One of Files' verbs or reads asked through a turn's tools that the agent's rules refuse it:
/// the words it is refused with.
async fn not_allowed(
    tools: &McpServer<WorkspaceActionProvider>,
    verb: &str,
    arguments: Value,
) -> String {
    let (text, failed) = on_turn(
        tools,
        "tilo_do",
        json!({"path": format!("files#{verb}"), "args": arguments}),
    )
    .await;
    assert!(failed, "files#{verb} was not refused: {text}");
    assert!(text.contains("not_permitted: "), "files#{verb}: {text}");
    text
}

/// A rule that answers `deny` for `kinds` of what Files keeps in `workspace`.
fn files_denied(
    workspace: &str,
    kinds: &[tilo_action_policy::ActionKind],
) -> Vec<tilo_action_policy::Rule> {
    let kinds = kinds
        .iter()
        .fold(tilo_action_policy::KindSet::NONE, |kinds, kind| {
            kinds.with(*kind)
        });
    vec![tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::workspace(workspace).app(FILES_APP),
        kinds,
        tilo_action_policy::Answer::Deny,
    )]
}

/// An agent on a turn the server carries out keeps and reads the workspace's folder through
/// that turn's tools, with the turn's bound: one nothing narrows makes the task's folder, puts
/// a note in it, lists it and reads the note back; one held to reading lists and reads and
/// changes nothing; one its rules refuse Files is refused every verb, the put and both reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_a_turn_keeps_and_reads_the_folder_as_its_rules_allow() {
    use tilo_action_policy::ActionKind::{Read, Undoable};
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (serving, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server.key.clone(),
            device_cert: server.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = serving.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // Ada's page keeps her device admitted; nothing is asked through it.
    let page = Browser::open_at(&serving, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    // The agent as a turn of Ada's carries it, held to `rules`.
    let conversation =
        chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let scout = |rules: Vec<tilo_action_policy::Rule>| {
        Caller::authenticated(
            root.verifying_key().to_bytes(),
            browser.cert.device_public_key,
        )
        .in_workspace(&workspace)
        .delegated_to(SCOUT.to_owned())
        .named("Scout")
        .in_agent_session("agent-session:files".to_owned())
        .on_turn(
            tilo_action_policy::turn::Turn::new(
                chat::summons::conversation_address(&conversation),
                "asked".to_owned(),
            ),
            rules,
        )
    };
    let tools = |rules: Vec<tilo_action_policy::Rule>| {
        McpServer::for_turn(
            WorkspaceActionProvider::new(actions.clone(), id),
            scout(rules),
        )
        .unwrap()
    };

    // An agent nothing narrows: the task's folder, a note put into it, the folder listed and
    // the note read back, all on the turn.
    let free = tools(Vec::new());
    let made = done(
        &free,
        "new-folder",
        json!({"parent": "Tasks", "name": "12 Plan the launch"}),
    )
    .await;
    assert!(made.contains(TASK), "{made}");
    let content = uploaded(&actions, id, &scout(Vec::new()), NOTES.as_bytes()).await;
    done(
        &free,
        "open",
        json!({"content": content, "name": "notes.md", "dir": TASK}),
    )
    .await;
    let listed = done(&free, "list", json!({"folder": TASK})).await;
    assert!(listed.contains("notes.md"), "{listed}");
    let file = format!("{TASK}/notes.md");
    let read = done(&free, "read-text", json!({"file": file})).await;
    assert!(read.contains("Book the hall."), "{read}");
    let renamed = done(
        &free,
        "rename",
        json!({"folder": TASK, "entry": "notes.md", "name": "Notes.md"}),
    )
    .await;
    assert!(renamed.contains("Notes.md"), "{renamed}");
    let tree = kept_when(&workspaces, &workspace, |tree| {
        file_at(tree, TASK, "Notes.md").is_some()
    })
    .await;
    assert!(file_at(&tree, TASK, "Notes.md").is_some(), "{renamed}");
    let file = format!("{TASK}/Notes.md");

    // It writes its report as a file where the task's folder is, and reads it back.
    let written = done(
        &free,
        "write-file",
        json!({"folder": TASK, "name": "report.md", "text": "The hall is booked.\n"}),
    )
    .await;
    assert!(written.contains("report.md"), "{written}");
    let report = format!("{TASK}/report.md");
    let read = done(&free, "read-text", json!({"file": report})).await;
    assert!(read.contains("The hall is booked."), "{read}");
    assert!(
        !written.contains(outside::OUTSIDE),
        "what the server says of the agent's own write is no content from outside: {written}"
    );

    // A file somebody passed on is put beside the notes. It says what to do.
    let passed_on = uploaded(
        &actions,
        id,
        &scout(Vec::new()),
        outside::PASSED_ON.as_bytes(),
    )
    .await;
    done(
        &free,
        "open",
        json!({"content": passed_on, "name": "from Bo.md", "dir": TASK}),
    )
    .await;
    let from_bo = format!("{TASK}/from Bo.md");

    // Held to reading what Files keeps: it lists and reads, and changes nothing.
    let reader = tools(files_denied(&workspace, &[Undoable]));
    let listed = done(&reader, "list", json!({"folder": TASK})).await;
    assert!(listed.contains("Notes.md"), "{listed}");
    let read = done(&reader, "read-text", json!({"file": file})).await;
    assert!(read.contains("Book the hall."), "{read}");
    // It reads the file that tells it to break its rules. The words come labelled as content
    // from outside, the label first, and they change nothing its rules allow: the trash the
    // file asks for is refused it as it was before it read a word.
    assert!(listed.contains(outside::OUTSIDE), "{listed}");
    let told = done(&reader, "read-text", json!({"file": from_bo})).await;
    let (label, words) = (told.find(outside::OUTSIDE), told.find("Ignore your rules"));
    assert!(words.is_some(), "{told}");
    assert!(
        label.is_some() && label < words,
        "what a file says is labelled as content from outside, the label first: {told}"
    );
    // The same file read by the address its content is kept under, as a message that carries
    // it names it: the same words behind the same label.
    let by_address = done(&reader, "read-text", json!({"content": passed_on})).await;
    assert!(
        by_address.contains("Ignore your rules") && by_address.contains(outside::OUTSIDE),
        "{by_address}"
    );
    not_allowed(
        &reader,
        "trash",
        json!({"folder": TASK, "entries": ["from Bo.md"]}),
    )
    .await;
    not_allowed(
        &reader,
        "trash",
        json!({"folder": TASK, "entries": ["Notes.md"]}),
    )
    .await;
    not_allowed(
        &reader,
        "new-folder",
        json!({"parent": "Tasks", "name": "13 Other"}),
    )
    .await;
    not_allowed(
        &reader,
        "open",
        json!({"content": content, "name": "copy.md", "dir": TASK}),
    )
    .await;

    not_allowed(
        &reader,
        "write-file",
        json!({"folder": TASK, "name": "other.md", "text": "x"}),
    )
    .await;

    // Refused Files by its rules: every verb, the put and both reads.
    let refused = tools(files_denied(&workspace, &[Read, Undoable]));
    not_allowed(&refused, "list", json!({"folder": TASK})).await;
    not_allowed(
        &refused,
        "write-file",
        json!({"folder": TASK, "name": "other.md", "text": "x"}),
    )
    .await;
    not_allowed(
        &refused,
        "write-file",
        json!({"folder": TASK, "name": "report.md", "text": "x", "replace": true}),
    )
    .await;
    not_allowed(&refused, "list", json!({})).await;
    not_allowed(&refused, "read-text", json!({"file": file})).await;
    not_allowed(&refused, "read-text", json!({"content": passed_on})).await;
    not_allowed(
        &refused,
        "new-folder",
        json!({"parent": "Tasks", "name": "13 Other"}),
    )
    .await;
    not_allowed(
        &refused,
        "move",
        json!({"from": TASK, "entries": ["Notes.md"], "to": "Elsewhere"}),
    )
    .await;
    not_allowed(
        &refused,
        "rename",
        json!({"folder": TASK, "entry": "Notes.md", "name": "Taken.md"}),
    )
    .await;
    not_allowed(
        &refused,
        "copy-into",
        json!({"from": TASK, "entries": ["Notes.md"], "to": "Elsewhere"}),
    )
    .await;
    not_allowed(
        &refused,
        "trash",
        json!({"folder": TASK, "entries": ["Notes.md"]}),
    )
    .await;
    not_allowed(&refused, "restore", json!({"entries": ["Notes.md"]})).await;
    not_allowed(
        &refused,
        "open",
        json!({"content": content, "name": "copy.md", "dir": TASK}),
    )
    .await;

    // Nothing either was refused changed the folder; the agent nothing narrows still reads it.
    let tree = tree_kept(&workspaces, &workspace).await;
    assert!(file_at(&tree, TASK, "Notes.md").is_some());
    assert!(
        file_at(&tree, TASK, "from Bo.md").is_some(),
        "the file that asked for the trash is where it was"
    );
    assert!(file_at(&tree, TASK, "copy.md").is_none());
    assert!(file_at(&tree, TASK, "other.md").is_none());
    assert_sealed_file(
        &serving,
        &workspace,
        &content_at(&tree, TASK, "report.md").unwrap(),
        b"The hall is booked.\n",
    )
    .await;
    assert!(file_at(&tree, TASK, "Taken.md").is_none());
    assert!(!folder_at(&tree, "Tasks", "13 Other") && !folder_at(&tree, "", "Elsewhere"));
    let listed = done(&free, "list", json!({"folder": TASK})).await;
    assert!(listed.contains("Notes.md"), "{listed}");
}

/// A file is written into the workspace's folder where its tree is kept, with no page open:
/// from text, and from content the workspace keeps, as an export another app answered is. It
/// writes over nothing unless asked to, and the call it answers takes it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_page_open_a_file_is_written_from_text_and_from_content_and_taken_back() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server alone: no page is opened, and no device of the person ever joins.
    let (server, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    let provider = WorkspaceActionProvider::new(actions.clone(), id);
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let write = |arguments: Value| asked(&provider, &person, &workspace, "write-file", arguments);
    let text_of = |file: &str| {
        let read = asked(
            &provider,
            &person,
            &workspace,
            "read-text",
            json!({"file": file}),
        );
        async move { read.await.unwrap()["text"].as_str().unwrap().to_owned() }
    };

    // A report written as text lands in the task's folder, which is made on the way, and
    // is named as a mounted computer names the same file.
    let report = format!("{TASK}/report.md");
    let written = write(json!({"folder": TASK, "name": "report.md", "text": NOTES}))
        .await
        .unwrap();
    assert_eq!(written["path"], report, "{written}");
    assert_eq!(written["size"], NOTES.len(), "{written}");
    assert_eq!(written["replaced"], false, "{written}");
    assert_eq!(written["made"], json!(["Tasks", TASK]), "{written}");
    assert!(
        written["invocation"].is_string(),
        "the call it was: {written}"
    );
    assert_eq!(text_of(&report).await, NOTES);
    let tree = kept_when(&workspaces, &workspace, |tree| {
        content_at(tree, TASK, "report.md").is_some()
    })
    .await;
    assert_sealed_file(
        &server,
        &workspace,
        &content_at(&tree, TASK, "report.md").unwrap(),
        NOTES.as_bytes(),
    )
    .await;

    // The name is taken: a second write is refused and the report is as it was.
    let moved = "Launch moved to Monday.\n";
    let said = write(json!({"folder": TASK, "name": "report.md", "text": moved}))
        .await
        .unwrap_err();
    assert!(said.contains("is there already"), "{said}");
    assert_eq!(text_of(&report).await, NOTES);
    // Asked to, it writes over it, and History's undo of that call puts back what it held.
    let over = write(json!({"folder": TASK, "name": "report.md", "text": moved, "replace": true}))
        .await
        .unwrap();
    assert_eq!(over["replaced"], true, "{over}");
    assert_eq!(over["made"], json!([]), "{over}");
    assert_eq!(
        over["entries"][0]["id"], written["entries"][0]["id"],
        "the same file"
    );
    assert_eq!(text_of(&report).await, moved);
    let back = asked(
        &provider,
        &person,
        &workspace,
        "undo",
        json!({"invocation": over["invocation"]}),
    )
    .await
    .unwrap();
    assert_eq!(back["taken"], 1, "{back}");
    assert_eq!(text_of(&report).await, NOTES);

    // What another app answers as content, here a PDF put into the workspace's store, is
    // written as a file by the same call, and the tree names it by its bytes.
    let pdf = form_pdf("Launch plan");
    let content = uploaded(&actions, id, &person, &pdf).await;
    let exports = format!("{TASK}/exports");
    let exported = write(json!({"folder": exports, "name": "plan.pdf", "content": content}))
        .await
        .unwrap();
    assert_eq!(
        exported["path"],
        format!("{exports}/plan.pdf"),
        "{exported}"
    );
    assert_eq!(exported["size"], pdf.len(), "{exported}");
    assert_eq!(exported["made"], json!([exports]), "{exported}");
    let tree = kept_when(&workspaces, &workspace, |tree| {
        content_at(tree, &exports, "plan.pdf").is_some()
    })
    .await;
    assert_sealed_file(
        &server,
        &workspace,
        &content_at(&tree, &exports, "plan.pdf").unwrap(),
        &pdf,
    )
    .await;
    // History's undo of the call takes the file back, with the folder made for it.
    asked(
        &provider,
        &person,
        &workspace,
        "undo",
        json!({"invocation": exported["invocation"]}),
    )
    .await
    .unwrap();
    let tree = kept_when(&workspaces, &workspace, |tree| {
        !folder_at(tree, TASK, "exports")
    })
    .await;
    assert!(!folder_at(&tree, TASK, "exports"), "{exported}");

    // A path never leaves the workspace's folder, one of text and content is named, the
    // content is the workspace's, and no device is named.
    let missing = format!("tilo.content:{}", "0".repeat(64));
    for (arguments, why) in [
        (
            json!({"folder": "../outside", "name": "x.md", "text": "x"}),
            "no name in the workspace",
        ),
        (
            json!({"folder": "/etc", "name": "x.md", "text": "x"}),
            "no leading /",
        ),
        (
            json!({"folder": TASK, "name": "a/b.md", "text": "x"}),
            "no file's name",
        ),
        (json!({"folder": TASK, "name": "x.md"}), "one of the two"),
        (
            json!({"folder": TASK, "name": "x.md", "text": "x", "content": content}),
            "one of the two",
        ),
        (
            json!({"folder": TASK, "name": "x.md", "content": "tilo.content:"}),
            "names no content",
        ),
        (
            json!({"folder": TASK, "name": "x.md", "content": missing}),
            "holds no",
        ),
        (
            json!({"device": "laptop", "folder": "/home/ada", "name": "x.md", "text": "x"}),
            "names no device",
        ),
    ] {
        let said = write(arguments.clone()).await.unwrap_err();
        assert!(said.contains(why), "{arguments}: {said}");
    }
    let listed = asked(
        &provider,
        &person,
        &workspace,
        "list",
        json!({"folder": TASK}),
    )
    .await
    .unwrap();
    assert_eq!(names_in(&listed), ["report.md"], "{listed}");
}

/// Invoke `action` on `app.so.tilo.file-explorer` (or `resource`) addressed to `served` with
/// `target.provider` set to `workspace:<named>` and optional `revision`, returning either its JSON
/// output value or the exact [`tilo_actions::remote::ActionCallError`] refusal.
async fn invoke_on_workspace_at_revision(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    served: tilo_server_protocol::WorkspaceId,
    caller: &Caller,
    named: &str,
    (resource, action): (&str, &str),
    arguments: Value,
    revision: Option<&str>,
) -> Result<Value, tilo_actions::remote::ActionCallError> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(named),
            address: ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: revision.map(str::to_owned),
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let invocation = request.invocation;
    let (actions, caller) = (actions.clone(), caller.clone());
    tokio::task::spawn_blocking(move || {
        let answer = actions
            .serve(served, &caller, RemoteActionMessage::Invoke(request))
            .expect("the service answers an invoke")
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the invoke answered in time");
        let RemoteActionMessage::Receipt(receipt) = answer else {
            panic!("expected a receipt, got {answer:?}");
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Rejected { error } => Err(error),
            ReceiptStatus::Accepted => {
                let started = std::time::Instant::now();
                loop {
                    match actions
                        .observe(&caller, &invocation)
                        .map(|state| state.update)
                    {
                        Some(InvocationUpdate::Succeeded { output, .. }) => {
                            return Ok(output.value.unwrap_or(Value::Null));
                        }
                        Some(other) if other.is_terminal() => {
                            panic!("expected completion or rejection, got {other:?}");
                        }
                        _ => {}
                    }
                    assert!(
                        started.elapsed() <= tilo_test_env::hang_bound(),
                        "the invocation did not settle in time"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            other => panic!("expected completion or rejection, got {other:?}"),
        }
    })
    .await
    .unwrap()
}

/// [`invoke_on_workspace_at_revision`] with no `revision`.
async fn invoke_on_workspace(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    served: tilo_server_protocol::WorkspaceId,
    caller: &Caller,
    named: &str,
    target: (&str, &str),
    arguments: Value,
) -> Result<Value, tilo_actions::remote::ActionCallError> {
    invoke_on_workspace_at_revision(actions, served, caller, named, target, arguments, None).await
}

/// App opens, tree reads, tree changes and `files` puts whose target names another workspace are
/// refused with `UnknownAction` before workspace-action dispatch or stale-revision checks and
/// leave the served workspace's tree untouched; the same requests naming the served workspace
/// behave as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_tree_requests_naming_another_workspace_are_refused() {
    use tilo_actions::remote::ActionCallError;

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let served = workspaces.create("Served").await.unwrap().workspace;
    let other = workspaces.create("Other").await.unwrap().workspace;
    let served_id = tilo_server_protocol::WorkspaceId::parse(&served).unwrap();
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(&served);
    let files_app = format!("app.{FILES_APP}");
    let writer_app = "app.so.tilo.word-processor";

    // Seed the served workspace with one text file and keep distinct blobs for refused and served puts.
    let seed_bytes = b"Served workspace note.\n";
    let seed_content = uploaded(&actions, served_id, &person, seed_bytes).await;
    let seeded = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "open"),
        json!({"content": seed_content, "name": "seed.txt", "dir": "Notes"}),
    )
    .await
    .unwrap();
    assert_eq!(seeded["path"], "Notes/seed.txt", "{seeded}");
    let seed_tree = kept_when(&workspaces, &served, |tree| {
        content_at(tree, "Notes", "seed.txt").is_some()
    })
    .await;
    let seed_hash = content_at(&seed_tree, "Notes", "seed.txt").unwrap();

    let refused_batch_bytes = b"Refused batch put body.\n";
    let refused_batch_content = uploaded(&actions, served_id, &person, refused_batch_bytes).await;
    let refused_single_bytes = b"Refused single put body.\n";
    let refused_single_content = uploaded(&actions, served_id, &person, refused_single_bytes).await;
    let batch_bytes = b"Batch put body.\n";
    let batch_content = uploaded(&actions, served_id, &person, batch_bytes).await;
    let single_bytes = b"Single put body.\n";
    let single_content = uploaded(&actions, served_id, &person, single_bytes).await;

    // Baseline: opening an app the catalogue holds is refused with UnknownAction when naming
    // another workspace and succeeds when naming the served workspace.
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (writer_app, "open"),
            json!({}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );
    let opened_writer = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (writer_app, "open"),
        json!({}),
    )
    .await
    .unwrap();
    let writer_record = opened_writer["record"].as_str().unwrap();
    assert!(!writer_record.is_empty(), "{opened_writer}");
    assert_eq!(
        opened_writer["provider"],
        format!("record:so.tilo.word-processor:{writer_record}"),
        "{opened_writer}"
    );

    // Tree reads naming another workspace are refused with UnknownAction, even with a stale
    // revision (whereas the same stale revision naming the served workspace returns StaleRevision).
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "list"),
            json!({"folder": "Notes"}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );
    assert_eq!(
        invoke_on_workspace_at_revision(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "list"),
            json!({"folder": "Notes"}),
            Some("stale-revision"),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );
    assert_eq!(
        invoke_on_workspace_at_revision(
            &actions,
            served_id,
            &person,
            &served,
            (&files_app, "list"),
            json!({"folder": "Notes"}),
            Some("stale-revision"),
        )
        .await,
        Err(ActionCallError::StaleRevision)
    );
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "read-text"),
            json!({"file": "Notes/seed.txt"}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );

    // Tree changes naming another workspace are refused with UnknownAction.
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "new-folder"),
            json!({"parent": "Notes", "name": "Intruder"}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "write-file"),
            json!({"folder": "Notes", "name": "intruder.md", "text": "wrong workspace"}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );

    // Both `files` put and single-file Files open naming another workspace are refused; each uses
    // folder/file names and content blobs distinct from the served puts below.
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "open"),
            json!({"files": [{"content": refused_batch_content, "name": "intruder-batch.txt", "dir": "RefusedDropped"}]}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );
    assert_eq!(
        invoke_on_workspace(
            &actions,
            served_id,
            &person,
            &other,
            (&files_app, "open"),
            json!({"content": refused_single_content, "name": "intruder-single.txt", "dir": "RefusedDropped"}),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );

    // The same three kinds of requests naming the served workspace succeed with exact answers.
    let listed = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "list"),
        json!({"folder": "Notes"}),
    )
    .await
    .unwrap();
    assert_eq!(listed["folder"], "Notes", "{listed}");
    assert_eq!(names_in(&listed), ["seed.txt"], "{listed}");
    assert_eq!(listed["total"], 1, "{listed}");

    let read = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "read-text"),
        json!({"file": "Notes/seed.txt"}),
    )
    .await
    .unwrap();
    assert_eq!(read["file"], "Notes/seed.txt", "{read}");
    assert_eq!(read["text"], "Served workspace note.\n", "{read}");
    assert_eq!(read["size"], seed_bytes.len(), "{read}");

    let made = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "new-folder"),
        json!({"parent": "Notes", "name": "Added"}),
    )
    .await
    .unwrap();
    assert_eq!(made["action"], "new-folder", "{made}");
    assert_eq!(made["entries"][0]["to"], "Notes/Added", "{made}");
    assert_eq!(made["entries"][0]["folder"], true, "{made}");

    let written_text = "Written in served.\n";
    let written = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "write-file"),
        json!({"folder": "Notes", "name": "written.md", "text": written_text}),
    )
    .await
    .unwrap();
    assert_eq!(written["action"], "write-file", "{written}");
    assert_eq!(written["path"], "Notes/written.md", "{written}");
    assert_eq!(written["size"], written_text.len(), "{written}");
    assert_eq!(written["replaced"], false, "{written}");

    let put_many = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "open"),
        json!({"files": [{"content": batch_content, "name": "batch.txt", "dir": "Dropped"}]}),
    )
    .await
    .unwrap();
    assert_eq!(put_many, json!({"count": 1}), "{put_many}");

    let put_one = invoke_on_workspace(
        &actions,
        served_id,
        &person,
        &served,
        (&files_app, "open"),
        json!({"content": single_content, "name": "single.txt", "dir": "Dropped"}),
    )
    .await
    .unwrap();
    assert_eq!(put_one["path"], "Dropped/single.txt", "{put_one}");
    assert_eq!(put_one["content"], single_content, "{put_one}");
    assert_eq!(put_one["size"], single_bytes.len(), "{put_one}");
    assert_eq!(put_one["replaced"], false, "{put_one}");

    let final_tree = kept_when(&workspaces, &served, |tree| {
        folder_at(tree, "Notes", "Added")
            && content_at(tree, "Notes", "written.md").is_some()
            && content_at(tree, "Dropped", "batch.txt").is_some()
            && content_at(tree, "Dropped", "single.txt").is_some()
    })
    .await;
    assert!(folder_at(&final_tree, "Notes", "Added"));
    assert_eq!(
        content_at(&final_tree, "Notes", "seed.txt"),
        Some(seed_hash)
    );
    assert_sealed_file(
        &server,
        &served,
        &content_at(&final_tree, "Notes", "written.md").unwrap(),
        written_text.as_bytes(),
    )
    .await;
    assert_sealed_file(
        &server,
        &served,
        &content_at(&final_tree, "Dropped", "batch.txt").unwrap(),
        batch_bytes,
    )
    .await;
    assert_sealed_file(
        &server,
        &served,
        &content_at(&final_tree, "Dropped", "single.txt").unwrap(),
        single_bytes,
    )
    .await;
    // None of the refused writes landed in the served workspace's tree or created the other's.
    assert!(!folder_at(&final_tree, "Notes", "Intruder"));
    assert!(file_at(&final_tree, "Notes", "intruder.md").is_none());
    assert!(!folder_at(&final_tree, "", "RefusedDropped"));
    assert!(file_at(&final_tree, "RefusedDropped", "intruder-batch.txt").is_none());
    assert!(file_at(&final_tree, "RefusedDropped", "intruder-single.txt").is_none());
    assert!(tree_if_kept(&workspaces, &other).await.is_none());
}

/// What the folder's files say and what the folder lists reach a turn's model labelled as content
/// from outside.
#[path = "put_files_outside.rs"]
mod outside;
