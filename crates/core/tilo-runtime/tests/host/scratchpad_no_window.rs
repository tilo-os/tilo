//! An agent invokes a Scratchpad action with no window open: Scratchpad's own package starts on
//! the call, the action changes the note the record keeps, a second call is answered by the same
//! process, and a caller the workspace holds no certificate for is refused at the dispatch, so
//! nothing starts for it.

use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, application_provider_token};
use tilo_actions::{ActionAddress, InvocationUpdate};
use tilo_resource::{EDIT_PARAM, FIELD_EDIT};

use super::{Agent, run_with_packages};

const SCRATCHPAD: &str = "so.tilo.scratchpad";
const NOTE: &str = "scratchpad.note";

/// The text the agent writes into the note.
const WRITTEN: &str = "Written with no window open";

/// The edit that replaces the note's body with `text`, as the field-edit action takes it.
fn body_edit(text: &str) -> Value {
    json!({"path": ["body"], "op": "replace", "text": text})
}

/// Run the field-edit that writes `text` into the note, as the agent, and answer whether it was
/// taken.
async fn write(agent: &Agent, text: &str) -> Value {
    let mut arguments = serde_json::Map::new();
    arguments.insert(EDIT_PARAM.to_owned(), body_edit(text));
    let reply = agent
        .tool_structured(
            "tilo_do",
            &json!({
                "path": format!("{SCRATCHPAD}/{NOTE}#{FIELD_EDIT}"),
                "args": arguments,
                "wait_ms": 30_000,
            }),
        )
        .await;
    let failed = reply["isError"].as_bool().unwrap_or(false);
    assert!(!failed, "the edit is taken: {}", reply["content"]);
    reply["structuredContent"]["result"].clone()
}

/// The note's body as the record holds it, read where `tilo_read record:` reads it.
async fn body(agent: &Agent) -> String {
    let reply = agent
        .tool_structured(
            "tilo_read",
            &json!({"path": format!("record:{SCRATCHPAD}:")}),
        )
        .await;
    let failed = reply["isError"].as_bool().unwrap_or(false);
    assert!(!failed, "the note reads: {}", reply["content"]);
    reply["structuredContent"]["records"][0]["value"]["body"]
        .as_str()
        .unwrap_or_else(|| panic!("the note holds its body: {reply}"))
        .to_owned()
}

#[test]
fn an_agent_invokes_a_scratchpad_action_with_no_window_open() {
    run_with_packages(&[], &[("scratchpad", "scratchpad")], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();

        // The first call starts the package with no window and changes its record.
        write(&agent, WRITTEN).await;
        assert_eq!(body(&agent).await, WRITTEN, "the edit reached the record");
        let processes = host
            .daemon()
            .apps()
            .no_window_processes(SCRATCHPAD, &workspace, None);
        assert_eq!(
            processes, 1,
            "Scratchpad runs one process with no window in {workspace}"
        );

        // A second call is answered by the same process: nothing starts for it.
        write(&agent, "Written again").await;
        assert_eq!(body(&agent).await, "Written again");
        assert_eq!(
            host.daemon()
                .apps()
                .no_window_processes(SCRATCHPAD, &workspace, None),
            processes,
            "the second call reused the process the first started"
        );

        // A caller the workspace holds no certificate for is refused at the dispatch, and starts
        // nothing.
        let visitor = host.visitor(&workspace).await;
        let mut arguments = serde_json::Map::new();
        arguments.insert(EDIT_PARAM.to_owned(), body_edit("Written by nobody"));
        let called = visitor
            .call(
                &application_provider_token(&workspace, SCRATCHPAD),
                ActionAddress::new(NOTE, FIELD_EDIT),
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
            body(&agent).await,
            "Written by nobody",
            "and nothing it wrote reached the note"
        );
        assert_eq!(
            host.daemon()
                .apps()
                .no_window_processes(SCRATCHPAD, &workspace, None),
            processes,
            "no process started for the caller without a grant"
        );
    });
}
