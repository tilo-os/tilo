//! What a turn on a workspace's server follows besides its own calls: an agent's `tilo_watch`
//! there wakes when a person writes in the workspace and reports the change, and it is never
//! told of what its rules keep from it.
use super::search::{narrow_agent, reading};
use super::shares::asks_scout;
use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionProvider as _, Caller};
use tilo_mcp::McpServer;
use tilo_workspace_service::agents::WorkspaceActionProvider;
use tilo_workspace_service::asks;

/// How long a watch here holds: far longer than the change takes to reach it, so an answer
/// inside [`PROMPTLY`] came on the change and not from the hold running out.
const HELD: u64 = 40_000;
/// How long the test lets the watch arm before a person writes.
const BEFORE: Duration = Duration::from_millis(100);
/// How long a watch may take to answer once it holds. Shorter than the bound the server puts
/// on a wait of its own (`MAX_REMOTE_WAIT`, ten seconds), so an answer inside it came on the
/// person's write and not on that bound running out.
const PROMPTLY: Duration = Duration::from_secs(8);

/// Call `name` with `arguments` on `server` and answer its result, as a client reads it.
async fn call(server: &McpServer<WorkspaceActionProvider>, name: &str, arguments: Value) -> Value {
    server
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .await
        .unwrap_or_else(|| panic!("{name} answers"))
}

/// One tool call's result as the tool gave it: its answer, parsed when it is JSON, and whether
/// it failed.
fn answered(answer: Value) -> (Value, bool) {
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
    (
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
        failed,
    )
}

/// [`call`], answered in time.
async fn tool(
    server: &McpServer<WorkspaceActionProvider>,
    name: &str,
    arguments: Value,
) -> (Value, bool) {
    answered(
        tokio::time::timeout(Duration::from_secs(30), call(server, name, arguments))
            .await
            .unwrap_or_else(|_| panic!("{name} answers in time")),
    )
}

/// Where the turn of the test is, and what asked it.
const TURN: (&str, &str) = ("plans", "ask");

/// The session of `root`'s person handed to her agent on a turn in `conversation`, held to
/// `rules`: what the tools of such a turn act as on a server.
fn on_turn(
    root: &SigningKey,
    browser: &crate::support::Device,
    workspace: &str,
    (conversation, cause): (&str, &str),
    rules: Vec<tilo_action_policy::Rule>,
) -> Caller {
    Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace)
    .delegated_to(SCOUT.to_owned())
    .named("Scout")
    .in_agent_session("agent-session:watch".to_owned())
    .on_turn(
        tilo_action_policy::turn::Turn::new(conversation.to_owned(), cause.to_owned()),
        rules,
    )
}

/// Every change one watch answered with: the app of each resource it names.
fn changed_apps(watched: &Value) -> Vec<String> {
    watched["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["kind"] == "change")
        .filter_map(|event| {
            event["resources"]
                .as_array()?
                .first()?
                .get("app")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_on_the_server_wakes_on_a_persons_write_and_is_told_only_what_it_may_read() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    // What she first asks the agent in the workspace's chat.
    let chat = asks::chat_packages().unwrap().app().to_owned();
    asks_scout(&workspaces, &workspace, &root, "what is planned?").await;
    let id = WorkspaceId::parse(&workspace).unwrap();

    // Nothing narrows the agent. Its watch holds until its person writes again, and then
    // reports her message: a change of the workspace, not one of the watch's own calls.
    let caller = on_turn(&root, &browser, &workspace, TURN, Vec::new());
    let tools = McpServer::for_turn(
        WorkspaceActionProvider::new(actions.clone(), id),
        caller.clone(),
    )
    .unwrap();
    let (started, failed) = tool(&tools, "tilo_watch", json!({"wait_ms": 50})).await;
    assert!(!failed, "the watch starts: {started}");
    assert_eq!(
        started["events"],
        json!([]),
        "a watch with no cursor starts now"
    );
    let cursor = started["cursor"]
        .as_str()
        .expect("the watch says where it read up to")
        .to_owned();
    let (watched, _said) = tokio::join!(
        tokio::time::timeout(
            PROMPTLY,
            call(
                &tools,
                "tilo_watch",
                json!({"cursor": cursor, "wait_ms": HELD})
            )
        ),
        async {
            tokio::time::sleep(BEFORE).await;
            asks_scout(&workspaces, &workspace, &root, "and who owns it?").await;
        }
    );
    let watched = watched
        .expect("the watch is woken by the person's message, not by its own hold running out");
    let (watched, failed) = answered(watched);
    assert!(!failed, "the watch answers: {watched}");
    assert!(
        changed_apps(&watched).contains(&chat),
        "the watch reports the person's message: {watched}"
    );

    // Narrowed so it may not read what Chat keeps, the same service is read with her next
    // message in it, and answers the agent nothing of Chat while her own device reads it: the
    // change is there, and it is the agent's rules that keep it from the agent.
    narrow_agent(
        &page,
        &workspaces,
        &workspace,
        &person,
        vec![reading(format!("workspace:{workspace}/app:{chat}"), "deny")],
    )
    .await;
    let narrowed = {
        let asking = Caller::authenticated(
            root.verifying_key().to_bytes(),
            browser.cert.device_public_key,
        )
        .in_workspace(&workspace)
        .delegated_to(SCOUT.to_owned())
        .named("Scout")
        .in_agent_session("agent-session:watch".to_owned());
        let rules = tilo_workspace_service::agents::workspace_agent(&workspaces, id, &asking).await;
        assert!(
            !rules.is_empty(),
            "the agent is held to the rules kept for it"
        );
        asking.on_turn(
            tilo_action_policy::turn::Turn::new(TURN.0.to_owned(), TURN.1.to_owned()),
            rules,
        )
    };
    let feed = WorkspaceActionProvider::new(actions.clone(), id)
        .change_feed()
        .expect("a turn on a server has its workspace's feed");
    let from = feed
        .since(&narrowed, None)
        .await
        .expect("the feed answers")
        .expect("the agent reads its workspace");
    asks_scout(&workspaces, &workspace, &root, "only I need this one").await;
    let herself = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let read = feed
            .since(&herself, Some(&from.cursor))
            .await
            .expect("the feed answers")
            .expect("her device reads the workspace");
        if read.changes.iter().any(|change| change.app == chat) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the server never reports her message: {:?}",
            read.changes
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let theirs = feed
        .since(&narrowed, Some(&from.cursor))
        .await
        .expect("the feed answers")
        .expect("the agent reads its workspace");
    assert!(
        theirs.changes.iter().all(|change| change.app != chat),
        "the agent is told nothing of what Chat keeps: {:?}",
        theirs.changes
    );
}
