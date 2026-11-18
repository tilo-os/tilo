//! Tool calls that ask for typed answers, as the agent tests make them.

use super::*;

/// A tool call that asks for typed answers, with `meta` merged into
/// `so.tilo.mcp`: its `structuredContent`, and whether it is an error.
pub(super) async fn typed<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
    meta: Value,
) -> (Value, bool) {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments,
                "_meta":{"so.tilo.mcp":{"structured":true,"invocation":meta}}}}))
        .await
        .unwrap();
    let result = &reply["result"];
    let structured = result["structuredContent"].clone();
    assert!(
        structured.is_object(),
        "{name} answered no typed value: {reply}"
    );
    assert_eq!(
        result["content"][0]["text"].as_str(),
        Some(structured.to_string().as_str()),
        "the text beside it is the same answer"
    );
    (structured, result["isError"].as_bool().unwrap_or_default())
}

/// The same, for a call that names no run.
pub(crate) async fn answer<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> (Value, bool) {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments,
                "_meta":{"so.tilo.mcp":{"structured":true}}}}))
        .await
        .unwrap();
    let result = &reply["result"];
    let structured = result["structuredContent"].clone();
    assert!(
        structured.is_object(),
        "{name} answered no typed value: {reply}"
    );
    (structured, result["isError"].as_bool().unwrap_or_default())
}
