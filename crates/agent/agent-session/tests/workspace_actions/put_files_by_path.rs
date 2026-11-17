//! Files' verbs by a path in the workspace, carried out where the workspace's tree is kept:
//! `files#new-folder`, `#move`, `#rename`, `#copy-into`, `#trash` and `#restore`.
//!
//! A workspace's folder is on no one disk, so these name a path in it and no device. A device's
//! agent asks them of the workspace through its session. With no page open and no device of
//! the person connected at all, the server's own action service answers them the same, which
//! is how an agent that works while its person's devices are off keeps a folder for each task.

use super::super::hosted_documents::ask;
use super::*;
use serde_json::Value;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate, api::ActionAddress};
use tilo_server::workspace_agents::WorkspaceActionProvider;
use tilo_server::workspaces::Workspaces;

/// The folder the work on one task keeps its material in.
const TASK: &str = "Tasks/12 Plan the launch";

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

    // Two files arrive in the workspace's folder.
    let visa = form_pdf("Application for Schengen Visa");
    let permit = form_pdf("Application for Residence Permit");
    for (name, bytes) in [("visa.pdf", &visa), ("permit.pdf", &permit)] {
        let content = content_in(&put(&server, bytes).await);
        files(&server, "open", json!({"content": content, "name": name})).await;
    }

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
        Some(manifest_of(&visa)),
        "the file is the same file in its new folder: {moved}"
    );
    assert_eq!(
        content_at(&tree, TASK, "permit.pdf"),
        Some(manifest_of(&permit))
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
        Some(manifest_of(&visa))
    );
    assert!(file_at(&tree, TASK, "visa.pdf").is_none());
    assert_eq!(
        content_at(&tree, TASK, "permit (2).pdf"),
        Some(manifest_of(&permit)),
        "{copied}"
    );
    assert_eq!(
        content_at(&tree, TASK, "permit.pdf"),
        Some(manifest_of(&permit)),
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
        Some(manifest_of(&permit))
    );

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
        Some(manifest_of(&permit)),
        "a refused verb changed nothing"
    );
    assert_eq!(
        content_at(&tree, TASK, "Visa form.pdf"),
        Some(manifest_of(&visa))
    );
    assert!(
        !folder_at(&tree, "", "elsewhere") && !folder_at(&tree, "", "home"),
        "no folder was made for a refused path"
    );
}

/// Ask the server's own action service for Files' `verb` on the folder of `workspace`, as
/// `caller`: what it answered, or the words it refused with.
async fn asked(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    verb: &str,
    arguments: Value,
) -> Result<Value, String> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: ActionAddress {
                resource: format!("app.{FILES_APP}"),
                action: verb.to_owned(),
            },
        },
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
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
    assert_eq!(
        made["undo"],
        json!({"action": "trash", "args": {"folder": "", "entries": ["Tasks"]}}),
        "one call takes it back: {made}"
    );
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
    assert_eq!(
        moved["undo"],
        json!({"action": "move", "args":
            {"from": TASK, "entries": ["drafts", "sources"], "to": "Inbox", "create": false}}),
        "{moved}"
    );
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

    // The trash takes a folder, and the call it answered brings it back.
    let gone = asked(
        &provider,
        &person,
        &workspace,
        "trash",
        json!({"folder": TASK, "entries": ["sources"]}),
    )
    .await
    .unwrap();
    assert_eq!(gone["undo"]["action"], "restore", "{gone}");
    let tree = kept_when(&workspaces, &workspace, |tree| {
        !folder_at(tree, TASK, "sources")
    })
    .await;
    assert!(!folder_at(&tree, TASK, "sources"), "{gone}");
    let back = asked(
        &provider,
        &person,
        &workspace,
        "restore",
        gone["undo"]["args"].clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        back["entries"][0]["to"],
        format!("{TASK}/sources"),
        "{back}"
    );

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
