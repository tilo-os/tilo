//! What the tests of a person's agent share: an MCP tool's answer, the listing and the call an
//! agent asks of the daemon, and the id its agent goes by.

use serde_json::{Value, json};

use crate::host;

/// The text an MCP tool's result answers, and whether it failed.
pub fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (
        if text.is_empty() {
            result.to_string()
        } else {
            text
        },
        result["isError"].as_bool().unwrap_or(false),
    )
}

/// `tilo_ls` of `path`, or of the whole workspace when `None`, as the agent reads it.
pub async fn ls(agent: &host::Agent, path: Option<&str>) -> (String, bool) {
    let arguments = path.map_or_else(|| json!({}), |path| json!({"path": path}));
    said(&agent.tool("tilo_ls", &arguments).await)
}

/// `tilo_do` of `path` with `args`, as the agent runs it.
pub async fn act(agent: &host::Agent, path: &str, args: Value) -> (String, bool) {
    said(
        &agent
            .tool("tilo_do", &json!({"path": path, "args": args}))
            .await,
    )
}

/// `tilo_watch` waiting `wait_ms`, as the agent follows who is here and what changed.
pub async fn watch(agent: &host::Agent, arguments: Value) -> (String, bool) {
    said(&agent.tool("tilo_watch", &arguments).await)
}

/// The kinds of the events a `tilo_watch` answer carries.
pub fn events(answered: &str) -> Vec<Value> {
    let parsed: Value = serde_json::from_str(answered).unwrap_or(Value::Null);
    parsed["events"].as_array().cloned().unwrap_or_default()
}

/// The id the computer's agent goes by, acting for the person whose root is `person` (hex): one
/// agent of theirs in every workspace, whichever client connects to it.
pub fn mcp_agent(person: &str) -> String {
    let root: [u8; 32] = hex::decode(person).unwrap().try_into().unwrap();
    tilo_actions::remote::device_agent_id(&root, "mcp")
}
