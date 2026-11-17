//! Programs on a workspace the relay hosts: the `tilo` tool and `POST …/programs` run a
//! tilolang program as the key's agent, compiled against the apps the workspace lists, and what
//! the program reads and changes goes through the same action API as every other call.

use super::api::{ISSUER, Mcp};
use super::api_on_relay::{OnRelay, api_on_relay, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};

/// `POST /v1/workspaces/{workspace}/programs` as `token`'s caller: the status and the typed
/// answer.
async fn programs(on_relay: &OnRelay, token: &str, workspace: &str, program: &str) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    json_of(
        on_relay
            .api
            .dispatch(
                &hyper::Method::POST,
                &format!("/v1/workspaces/{workspace}/programs"),
                "",
                &headers(&[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ]),
                json!({"program": program}).to_string().as_bytes(),
            )
            .await,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_program_runs_on_a_workspace_the_relay_hosts() {
    let on_relay = api_on_relay(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let mut account = Mcp {
        api: &on_relay.api,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let created = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Applicant: Jane Doe"}}))
        .await;
    let workspace = created["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {created}"))
        .to_owned();
    let mut session = Mcp {
        api: &on_relay.api,
        path: format!("/w/{workspace}/mcp"),
        token: key.clone(),
        session: None,
    };
    let (status, init) = session.initialize().await;
    assert_eq!(status, 200, "{init}");
    let tilo = |program: &str| {
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "tilo", "arguments": {"program": program}}})
    };

    // A program reads the workspace's app: the notebook the app keeps, named through its
    // declarations, which the workspace's host publishes.
    let (status, read) = session.post(tilo("notes.notebook")).await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["result"]["isError"], false, "{read}");

    // `/programs`, as `tilo run` reaches it, changes the record through the app's own action,
    // as one step.
    let (status, made) = programs(
        &on_relay,
        &key,
        &workspace,
        "notes.new_note(notes.notebook, title: \"From the hosted task\")",
    )
    .await;
    assert_eq!(status, 200, "{made}");
    assert_eq!(made["status"], "ok", "{made}");
    assert_eq!(made["changed"], 1, "{made}");
    assert_eq!(made["steps"].as_array().map(Vec::len), Some(1), "{made}");

    // The note is in the workspace's record, read as a person's call reads it.
    let (status, read) = session
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_read", "arguments": {"path": "notes"}}}))
        .await;
    assert_eq!(status, 200, "{read}");
    assert!(
        read.to_string().contains("From the hosted task"),
        "the note the program made is in the workspace: {read}"
    );

    // Another note through the session's own tool: its step is the task's history, so `undo`
    // takes it back.
    let (status, made) = session
        .post(tilo(
            "notes.new_note(notes.notebook, title: \"Also from the task\")",
        ))
        .await;
    assert_eq!(status, 200, "{made}");
    assert_eq!(made["result"]["isError"], false, "{made}");
    let (status, history) = session.post(tilo("history task")).await;
    assert_eq!(status, 200, "{history}");
    assert!(
        history.to_string().contains("Also from the task"),
        "the step the program took is the task's history: {history}"
    );
}
