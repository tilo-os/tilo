//! An agent invokes a Console action with no window open: Console's own package starts on the
//! call, the action changes the record it keeps, a second call is answered by the same process,
//! and a caller the workspace holds no certificate for is refused at the dispatch, so nothing
//! starts for it.

use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, application_provider_token};
use tilo_actions::{ActionAddress, InvocationUpdate};
use tilo_resource::{EDIT_PARAM, FIELD_EDIT};

use super::{Agent, run_with_packages};

const CONSOLE: &str = "so.tilo.console";
const VIEW: &str = "console.view";

/// The text the agent writes into the record.
const WRITTEN: &str = "Written with no window open";

/// The edit that replaces the record's agent text with `text`, as the field-edit action takes it.
fn agent_edit(text: &str) -> Value {
    json!({"path": ["agent"], "op": "replace", "text": text})
}

/// Run the field-edit that writes `text` into the record, as the agent, and answer whether it
/// was taken.
async fn write(agent: &Agent, text: &str) -> Value {
    let mut arguments = serde_json::Map::new();
    arguments.insert(EDIT_PARAM.to_owned(), agent_edit(text));
    let reply = agent
        .tool_structured(
            "tilo_do",
            &json!({
                "path": format!("{CONSOLE}/{VIEW}#{FIELD_EDIT}"),
                "args": arguments,
                "wait_ms": 30_000,
            }),
        )
        .await;
    let failed = reply["isError"].as_bool().unwrap_or(false);
    assert!(!failed, "the edit is taken: {}", reply["content"]);
    reply["structuredContent"]["result"].clone()
}

/// The record's agent text as it holds it, read where `tilo_read record:` reads it.
async fn agent_text(agent: &Agent) -> String {
    let reply = agent
        .tool_structured(
            "tilo_read",
            &json!({"path": format!("record:{CONSOLE}:")}),
        )
        .await;
    let failed = reply["isError"].as_bool().unwrap_or(false);
    assert!(!failed, "the record reads: {}", reply["content"]);
    reply["structuredContent"]["records"][0]["value"]["agent"]
        .as_str()
        .unwrap_or_else(|| panic!("the record holds its agent text: {reply}"))
        .to_owned()
}

#[test]
fn an_agent_invokes_a_console_action_with_no_window_open() {
    run_with_packages(&[], &[("console", "console")], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();

        // The first call starts the package with no window and changes its record.
        write(&agent, WRITTEN).await;
        assert_eq!(agent_text(&agent).await, WRITTEN, "the edit reached the record");
        let processes = host
            .daemon()
            .apps()
            .no_window_processes(CONSOLE, &workspace, None);
        assert_eq!(
            processes, 1,
            "Console runs one process with no window in {workspace}"
        );

        // A second call is answered by the same process: nothing starts for it.
        write(&agent, "Written again").await;
        assert_eq!(agent_text(&agent).await, "Written again");
        assert_eq!(
            host.daemon()
                .apps()
                .no_window_processes(CONSOLE, &workspace, None),
            processes,
            "the second call reused the process the first started"
        );

        // A caller the workspace holds no certificate for is refused at the dispatch, and starts
        // nothing.
        let visitor = host.visitor(&workspace).await;
        let mut arguments = serde_json::Map::new();
        arguments.insert(EDIT_PARAM.to_owned(), agent_edit("Written by nobody"));
        let called = visitor
            .call(
                &application_provider_token(&workspace, CONSOLE),
                ActionAddress::new(VIEW, FIELD_EDIT),
                &Value::Object(arguments),
            )
            .await;
        assert!(
            matches!(
                called,
                Err(ActionCallError::NotPermitted
                    | ActionCallError::Refused { .. }
                    | ActionCallError::UnknownAction)
                    | Ok(InvocationUpdate::Failed { .. })
            ),
            "a caller with no grant is refused: {called:?}"
        );
        assert_ne!(
            agent_text(&agent).await,
            "Written by nobody",
            "and nothing it wrote reached the record"
        );
        assert_eq!(
            host.daemon()
                .apps()
                .no_window_processes(CONSOLE, &workspace, None),
            processes,
            "no process started for the caller without a grant"
        );
    });
}
