//! A computer's peer that starts again (`tilo` as a user service, restarted) is asked at once:
//! `tilo migrate start` opens Tasks and makes issues the moment the peer answers. Each call goes
//! through on its first ask, into a tracker no one wrote an issue in yet as into one that holds
//! some, and nothing asks the caller to ask again.
use super::computer_agent::computer_agent;
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

const SHEETS: &str = "so.tilo.sheets";

/// What the agent's `tilo_do` of `path` with `args` answered, once it answered with what it did.
async fn act(server: &McpServer<SessionActions>, path: &str, args: Value) -> Value {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}}))
        .await
        .unwrap();
    assert_ne!(answer["result"]["isError"], true, "{path}: {answer}");
    answer
}

fn issues(from: usize, count: usize) -> Value {
    let issues: Vec<Value> = (from..from + count)
        .map(
            |at| json!({"title": format!("Issue {at}"), "status": "todo", "labels": ["Migration"]}),
        )
        .collect();
    json!({ "issues": issues })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_computers_first_calls_go_through() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &server).await;
    browser.create(SHEETS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    // The peer as it ran before: Tasks opened, and the first issues made in it.
    let (before, mcp) = computer_agent(&hosted, &laptop, &server).await;
    act(&mcp, "tasks#open", json!({})).await;
    act(&mcp, "tasks/tasks.issues#create-many", issues(0, 2)).await;
    drop(mcp);
    before.peer.close();
    drop(before);

    // Started again, and asked the moment it is in the workspace.
    let (_after, mcp) = computer_agent(&hosted, &laptop, &server).await;
    act(&mcp, "tasks#open", json!({})).await;
    act(&mcp, "tasks/tasks.issues#create-many", issues(2, 2)).await;
}
