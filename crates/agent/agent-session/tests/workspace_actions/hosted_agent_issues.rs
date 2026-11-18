//! An agent on a workspace's server works on the issue a person gives it through the agent
//! tools: it finds the issue where Tasks keeps it (a part of the tracker's chain), reads it,
//! comments on it and moves it on, as a person's page does.
use super::hosted_agent_turns::{SCOUT, TASKS, file_issue, make_agent, tracker};

/// The app that keeps notes.
const NOTES: &str = "so.tilo.notes";
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_server::workspace_agents::WorkspaceActionProvider;

/// Call `name` with `arguments` on `server`: its text, and whether it failed.
async fn tool(
    server: &McpServer<WorkspaceActionProvider>,
    name: &str,
    arguments: Value,
) -> (String, bool) {
    let answer = tokio::time::timeout(
        Duration::from_secs(30),
        server.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })),
    )
    .await
    .unwrap_or_else(|_| panic!("{name} answers in time: {arguments}"))
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

/// The rows `tilo_ls` answers for a path, as a program reads them: each
/// resource's own address among them.
macro_rules! rows {
    ($server:expr, $path:expr) => {{
        let answer = tokio::time::timeout(
            Duration::from_secs(30),
            $server.handle(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "tilo_ls",
                    "arguments": {"path": $path},
                    "_meta": {"so.tilo.mcp": {"structured": true}},
                },
            })),
        )
        .await
        .expect("tilo_ls answers in time")
        .expect("tilo_ls answers");
        answer["result"]["structuredContent"]["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }};
}

/// The person files "Plan the launch" for the agent in a Tasks window of their own page, as a
/// browser does: the workspace's tracker and the part of the issues it filed it in reach the
/// workspace as the page's writes. The part.
async fn file_in_page(
    page: &Browser,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
) -> String {
    let mut window = crate::common::open_in(
        workspace,
        TASKS,
        &MemberId::person(person),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    window
        .invoke(
            &ActionAddress::new(
                ui::resource::chain_resource_id(&tasks::issue_list::chain()),
                "create",
            ),
            json!({"title": "Plan the launch", "assignee": format!("agent:{SCOUT}")})
                .as_object()
                .unwrap(),
            false,
        )
        .expect("the person files the issue");
    let route = |document: &str| DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.into(),
        document: document.to_owned(),
    };
    // Tasks keeps one tracker per workspace, which travels with no id of its own.
    if let Some(written) = window.take_mutation().unwrap() {
        page.peer.write(route(""), &written).unwrap();
    }
    for (part, written) in window.take_document_mutations().unwrap() {
        page.peer.write(route(part.as_str()), &written).unwrap();
    }
    let part = tasks::issue_list::chain().part(0).as_str().to_owned();
    let deadline = tilo_test_env::hang_deadline();
    while workspaces
        .resource_document(workspace, TASKS, &part)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the server never keeps the issue"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    part
}

/// The person writes "Launch checklist" in Notes through the server's action service, as their
/// page does. The notebook's document for the workspace.
async fn write_note(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    (governor, browser): (&crate::support::Device, &crate::support::Device),
) -> String {
    let catalogue = crate::common::catalogue();
    let notebook = tilo_server::workspace_bots::workspace_resource(&catalogue, NOTES);
    workspaces
        .prepare_application(workspace, &governor.cert, &catalogue, NOTES, &notebook)
        .await
        .unwrap();
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = WorkspaceActionProvider::new(actions.clone(), id);
    let person = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace);
    let route = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.into(),
        document: notebook,
    };
    tilo_server::workspace_agents::invoke_named(
        &service,
        &person,
        &route,
        &notes::note_list::chain_resource(),
        "create-page",
        json!({"title": "Launch checklist"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the person writes a note");
    route.document
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_server_works_on_an_issue_filed_through_its_actions() {
    Box::pin(works_on_its_issue(false, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_server_works_on_an_issue_filed_in_a_page() {
    Box::pin(works_on_its_issue(true, false)).await;
}

/// A public workspace sends a device its documents only once it opens them: the server's own
/// member of it holds none of the tracker or its issues until its agent's tools ask for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_server_works_on_an_issue_filed_in_a_public_workspace() {
    Box::pin(works_on_its_issue(true, true)).await;
}

/// The agent finds the issue given to it, reads it, comments on it and moves it to In review,
/// through the tools the server's turn opens; `in_page`: the issue was filed in the person's own
/// Tasks window rather than through the server's actions; `public`: in a workspace listed for
/// anyone.
async fn works_on_its_issue(in_page: bool, public: bool) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (serving, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = &serving.workspaces().unwrap().clone();
    let workspace = &workspaces.create("Team").await.unwrap().workspace;
    if public {
        workspaces
            .publish(
                workspace,
                &server.cert,
                tilo_server_protocol::directory::Listing {
                    description: "Everyone".into(),
                    default_role: tilo_peer_state::join::Role::Editor,
                },
            )
            .await
            .unwrap();
    }
    let page = Browser::open_at(&serving, &browser, &server, workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, workspaces, workspace, &person, None).await;
    let part = if in_page {
        file_in_page(&page, workspaces, workspace, &person).await
    } else {
        file_issue(&actions, workspaces, workspace, &root, (&server, &browser)).await
    };
    let filed = tracker(workspaces, workspace, &part).await;
    write_note(&actions, workspaces, workspace, &root, (&server, &browser)).await;
    let (issue, _) = filed.issues.iter().next().unwrap();
    let issue = issue.clone();
    let path = tasks::model::issue_resource(&issue);

    // The agent's tools, as the server's turn for the person who gave it the issue opens them.
    let id = WorkspaceId::parse(workspace).unwrap();
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace)
    .delegated_to(SCOUT.to_owned())
    .named("Scout")
    .in_agent_session("agent-session:issue".to_owned())
    .on_turn(
        tilo_action_policy::turn::Turn::new(path.clone(), "assigned".to_owned()),
        Vec::new(),
    );
    let tools = WorkspaceActionProvider::new(actions.clone(), id);
    let server = McpServer::for_turn(tools, caller).unwrap();

    let (listed, failed) = tool(&server, "tilo_ls", json!({"path": "tasks/Issues"})).await;

    assert!(
        !failed && listed.contains("Plan the launch"),
        "the agent finds its issue: {listed}"
    );
    let (read, failed) = tool(&server, "tilo_read", json!({"path": path})).await;

    assert!(
        !failed && read.contains("Plan the launch"),
        "the agent reads its issue: {read}"
    );

    // Every address the tools show for the issue is an address the next call
    // takes: the collection the app lists, the issue under it, the item's own
    // address that a prompt names, and the number a person reads it by.
    let collection = rows!(&server, "tasks")
        .into_iter()
        .find(|row| row["name"] == "Issues")
        .and_then(|row| row["path"].as_str().map(str::to_owned))
        .expect("tilo_ls tasks lists the issues' collection");
    let shown = rows!(&server, &collection)
        .into_iter()
        .find(|row| row["name"] == "Plan the launch")
        .and_then(|row| row["path"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("tilo_ls {collection} lists the issue"));
    assert_eq!(
        shown,
        format!("{collection}/Plan the launch"),
        "the issue's listed address is the one under the collection it was listed in"
    );
    let (read, failed) = tool(&server, "tilo_read", json!({"path": shown})).await;
    assert!(
        !failed && read.contains("Plan the launch"),
        "the listed address reads: {read}"
    );
    let (seen, failed) = tool(&server, "tilo_view", json!({"path": shown})).await;
    assert!(
        !failed && seen.contains("Plan the launch"),
        "the listed address shows: {seen}"
    );
    let (acted, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{shown}#comment"),
            "args": {"text": "Read off the listing."},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(
        !failed && acted.contains("ok"),
        "the listed address takes an action: {acted}"
    );
    // The item's own address a prompt names, with the kind and the id its
    // listing prints, and the number a person names it by.
    for address in [format!("tasks.issues/{}", issue.as_str()), "#1".to_owned()] {
        let (read, failed) = tool(&server, "tilo_read", json!({"path": address})).await;
        assert!(
            !failed && read.contains("Plan the launch"),
            "{address} reads the issue: {read}"
        );
        let (seen, failed) = tool(&server, "tilo_view", json!({"path": address})).await;
        assert!(
            !failed && seen.contains("Plan the launch"),
            "{address} shows the issue: {seen}"
        );
    }
    let (acted, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "#1#comment", "args": {"text": "On it."}, "wait_ms": 30_000}),
    )
    .await;
    assert!(
        !failed && acted.contains("ok"),
        "the number a person names the issue by takes an action: {acted}"
    );
    // What the workspace's search names the issue by is an address too.
    let (found, failed) = tool(&server, "tilo_find", json!({"query": "#1"})).await;
    let found = found
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .filter(|_| !failed)
        .unwrap_or_else(|| panic!("the search finds the issue by its number: {found}"))
        .to_owned();
    let (read, failed) = tool(&server, "tilo_read", json!({"path": found})).await;
    assert!(
        !failed && read.contains("Plan the launch"),
        "the address the search names reads: {read}"
    );
    let (acted, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{found}#comment"),
            "args": {"text": "Found by the search."},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(
        !failed && acted.contains("ok"),
        "the address the search names takes an action: {acted}"
    );
    // An address that names nothing says which address works instead.
    for wrong in [
        format!("{collection}/no such issue"),
        "tasks.issues/issues/no-such-issue".to_owned(),
    ] {
        let (said, failed) = tool(&server, "tilo_read", json!({"path": wrong})).await;
        assert!(failed, "{wrong} names no issue: {said}");
        assert!(
            said.contains("it has:") && !said.ends_with("it has: "),
            "{wrong} says what there is to name: {said}"
        );
    }
    let (wrong, _) = tool(
        &server,
        "tilo_read",
        json!({"path": format!("{collection}/no such issue")}),
    )
    .await;
    assert!(
        wrong.contains("it has:") && wrong.contains(&shown),
        "an address that fails names one that works ({shown}): {wrong}"
    );

    // A resource one tool showed goes into another action's reference argument
    // the same way: the note's listed address links it, no encoding by hand.
    let pages = rows!(&server, "notes")
        .into_iter()
        .find(|row| row["has_children"] == Value::Bool(true) && row["kind"] == "notes.pages")
        .and_then(|row| row["path"].as_str().map(str::to_owned))
        .expect("tilo_ls notes lists where its pages are");
    let note = rows!(&server, &pages)
        .into_iter()
        .find(|row| row["name"] == "Launch checklist")
        .and_then(|row| row["path"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("tilo_ls {pages} lists the note"));
    let (linked, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#link-note"),
            "args": {"issue": issue.as_str(), "note": note},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(!failed, "the note's listed address links it: {linked}");
    let linked_issue = tracker(workspaces, workspace, &part).await;
    assert!(
        linked_issue.issues.get(&issue).is_some_and(|now| now
            .links
            .values()
            .any(|link| link.label == "Launch checklist")),
        "the issue links the note: {:?}",
        linked_issue.issues.get(&issue).map(|now| &now.links)
    );

    // A document by the id its address carries is read through the read API
    // the same way, answered by the workspace's own service.
    let (copy, failed) = tool(
        &server,
        "tilo_read",
        json!({"path": format!("record:{TASKS}:{part}")}),
    )
    .await;
    assert!(
        !failed && copy.contains("Plan the launch"),
        "the document the issues are kept in reads: {copy}"
    );
    // `*` is every field a listing knows, as a caller with no screen writes it.
    let (all, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "tasks/Issues#read", "args": {"fields": ["*"]}}),
    )
    .await;
    assert!(!failed, "a read of every field answers: {all}");
    assert!(all.contains("Plan the launch"), "{all}");
    // An item of a chain reads whole by its own id, in the part the chain keeps it in.
    let (item, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": "tasks/Issues#read",
            "args": {"item": path},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(
        !failed && item.contains("Plan the launch"),
        "the issue reads as an item of its chain: {item}"
    );
    // The note's reference as a listing row carries it (its id alone) links it too, and a
    // reference that lacks everything says what it needs and where to get it, all at once.
    let note_id = tracker(workspaces, workspace, &part)
        .await
        .issues
        .get(&issue)
        .and_then(|now| {
            now.links
                .values()
                .find(|link| link.label == "Launch checklist")
                .map(|link| link.resource.clone())
        })
        .expect("the issue lists the note it links");
    let (linked, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#link-note"),
            "args": {
                "issue": issue.as_str(),
                "note": {"id": note_id},
            },
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(!failed, "a reference by its id links the note: {linked}");
    let (refused, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#link-note"),
            "args": {"issue": issue.as_str(), "note": {"label": "Launch checklist"}},
        }),
    )
    .await;
    assert!(
        failed,
        "a reference that lacks its fields is refused: {refused}"
    );
    for needed in ["app", "kind", "id", "label", "tilo_ls"] {
        assert!(
            refused.contains(needed),
            "the refusal names {needed} and where to get it: {refused}"
        );
    }
    // How an argument is written makes it nothing else: where an action declares a note, a
    // text spelled as the reference of a secret names no resource and is refused as one.
    let links =
        |list: &tasks::issue_list::IssueList| list.issues.get(&issue).map(|now| now.links.len());
    let before = links(&tracker(workspaces, workspace, &part).await);
    let (refused, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#link-note"),
            "args": {
                "issue": issue.as_str(),
                "note": format!("vault.item:{}", "0123456789abcdef".repeat(2)),
            },
        }),
    )
    .await;
    assert!(
        failed && refused.contains("is a resource of the workspace"),
        "a secret's reference where a note is picked is refused as a path that names no \
         resource: {refused}"
    );
    assert_eq!(
        links(&tracker(workspaces, workspace, &part).await),
        before,
        "nothing was linked"
    );
    // The number a listing shows an issue by is an address of its own.
    let (numbered, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": format!("{collection}/1")}),
    )
    .await;
    assert!(
        !failed && numbered.contains("Plan the launch"),
        "the issue's number is an address: {numbered}"
    );
    // A status written as a person reads it is the status the app takes.
    let (placed, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#place"),
            "args": {"issue": issue.as_str(), "status": "In review"},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(!failed, "a status written as it is read is taken: {placed}");

    let (commented, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#comment"),
            "args": {"text": "On it: the plan is drafted."},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(!failed, "the agent comments: {commented}");
    let (moved, failed) = tool(
        &server,
        "tilo_do",
        json!({
            "path": format!("{path}#place"),
            "args": {"status": "in-review"},
            "wait_ms": 30_000,
        }),
    )
    .await;
    assert!(!failed, "the agent moves the issue: {moved}");

    // The addresses a prompt names, on a workspace whose Tasks has only ever been served:
    // the kind, its items' collection, and one issue by its item address all reach the chain.
    for address in [
        path.as_str(),
        "tasks.issues.issues",
        "tasks.issues/issues",
        "tasks.issues",
    ] {
        let (said, failed) = tool(&server, "tilo_read", json!({"path": address})).await;
        assert!(
            !failed,
            "{address} is no app: the kind reaches the issues (it answered {said})"
        );
    }
    for address in ["tasks.issues.issues", "tasks.issues/issues"] {
        let (said, failed) = tool(&server, "tilo_ls", json!({"path": address})).await;
        assert!(
            !failed && said.contains("Plan the launch"),
            "{address} lists: {said}"
        );
    }
    // An issue named by its number, as a listing and a person write it: the integer the model
    // sends, the text, and the `#1` a person reads.
    for issue_number in [json!(1), json!("1"), json!("#1")] {
        let (read, failed) = tool(
            &server,
            "tilo_do",
            json!({
                "path": "tasks.issues#assign",
                "args": {"issue": issue_number, "assignee": format!("agent:{SCOUT}")},
                "wait_ms": 30_000,
            }),
        )
        .await;
        assert!(
            !failed,
            "the issue's number names it as its address does ({issue_number}): {read}"
        );
    }
    // An issue named by its number, as a listing and a person write it: the integer a model
    // sends, the text, and the `#1` a person reads.
    for issue_number in [json!(1), json!("1"), json!("#1")] {
        let (assigned, failed) = tool(
            &server,
            "tilo_do",
            json!({
                "path": "tasks.issues#assign",
                "args": {"issue": issue_number, "assignee": format!("agent:{SCOUT}")},
                "wait_ms": 30_000,
            }),
        )
        .await;
        assert!(
            !failed,
            "the issue's number names it as its address does ({issue_number}): {assigned}"
        );
    }

    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = tracker(workspaces, workspace, &part).await;
        let now = held.issues.get(&issue).unwrap();
        let said = now
            .comments
            .values()
            .any(|comment| comment.text.contains("the plan is drafted"));
        if said && now.status == tasks::model::Status::InReview {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the issue holds the agent's comment and status: {:?} {:?}",
            now.status,
            now.comments
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The names a prompt writes for the issues reach them on a workspace whose Tasks has only ever
/// been served, never opened: the kind and its collections (`tasks.issues`, `tasks.issues.issues`,
/// `tasks.issues/issues`), one issue by its item address, and one by the number the UI shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_reads_the_issues_by_every_name_a_prompt_writes() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (serving, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = &serving.workspaces().unwrap().clone();
    let workspace = &workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&serving, &browser, &server, workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, workspaces, workspace, &person, None).await;
    let part = file_issue(&actions, workspaces, workspace, &root, (&server, &browser)).await;
    let filed = tracker(workspaces, workspace, &part).await;
    let (issue, _) = filed.issues.iter().next().unwrap();
    let path = tasks::model::issue_resource(issue);

    let id = WorkspaceId::parse(workspace).unwrap();
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace)
    .delegated_to(SCOUT.to_owned())
    .named("Scout")
    .in_agent_session("agent-session:issue".to_owned())
    .on_turn(
        tilo_action_policy::turn::Turn::new(path.clone(), "assigned".to_owned()),
        Vec::new(),
    );
    let tools = WorkspaceActionProvider::new(actions.clone(), id);
    let server = McpServer::for_turn(tools, caller).unwrap();

    for address in [
        path.as_str(),
        "tasks.issues.issues",
        "tasks.issues/issues",
        "tasks.issues",
    ] {
        let (said, failed) = tool(&server, "tilo_read", json!({"path": address})).await;
        assert!(!failed, "the kind `{address}` reaches the issues: {said}");
    }
    for address in ["tasks.issues.issues", "tasks.issues/issues"] {
        let (said, failed) = tool(&server, "tilo_ls", json!({"path": address})).await;
        assert!(
            !failed && said.contains("Plan the launch"),
            "`{address}` lists the issues: {said}"
        );
    }
    // The number a listing and a person write for one issue.
    for address in ["tasks/Issues/1", "tasks.issues/issues/1", "#1"] {
        let (read, failed) = tool(&server, "tilo_read", json!({"path": address})).await;
        assert!(
            !failed && read.contains("Plan the launch"),
            "`{address}` reads the issue: {read}"
        );
    }
}
