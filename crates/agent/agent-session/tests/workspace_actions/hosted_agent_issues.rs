//! An agent on a workspace's server works on the issue a person gives it through the agent
//! tools: it finds the issue where Tasks keeps it (a part of the tracker's chain), reads it,
//! comments on it and moves it on, as a person's page does.
use super::hosted_agent_turns::{SCOUT, TASKS, file_issue, make_agent, tracker};
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
                ui::resource::chain_resource_id(&tasks::part::chain()),
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
    let part = tasks::part::chain().part(0).as_str().to_owned();
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

    let (commented, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": format!("{path}#comment"), "args": {"text": "On it: the plan is drafted."}}),
    )
    .await;
    assert!(!failed, "the agent comments: {commented}");
    let (moved, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": format!("{path}#place"), "args": {"status": "in-review"}}),
    )
    .await;
    assert!(!failed, "the agent moves the issue: {moved}");

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
