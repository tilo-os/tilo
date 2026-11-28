//! What the tests of a person's agent share: an MCP tool's answer, the listing and the call an
//! agent asks of the daemon, and the id its agent goes by.

use serde_json::{Value, json};

use crate::language::host;

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

/// `terminal#open-terminal` of `args`, as the agent asks it under `invocation`: a terminal a
/// program types into reaches beyond the workspace's records, so its person is asked first. The
/// person allows it on their own connection, as `tilo`'s prompt answers it, and the call's status
/// once it has started, which names the stream to connect at.
pub async fn open_terminal_allowed(
    host: &host::Host,
    agent: &host::Agent,
    invocation: &str,
    args: Value,
) -> String {
    let (asked, _) = said(
        &agent
            .tool(
                "tilo_do",
                &json!({"path": "terminal#open-terminal", "args": args,
                        "invocation_id": invocation}),
            )
            .await,
    );
    assert!(
        asked.contains("asked") && !asked.contains("stream"),
        "the person is asked before a program gets a terminal: {asked}"
    );
    allow(host, &asked).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (started, _) = said(
            &agent
                .tool("tilo_status", &json!({"invocation_id": invocation}))
                .await,
        );
        if started.contains("stream") {
            return started;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the terminal opens once its person allowed it: {started}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The person allows the call `asked` says waits for them, on their own connection.
async fn allow(host: &host::Host, asked: &str) {
    use tilo_actions::remote::ActionProvider as _;
    let record = asked
        .split("It waits in ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .and_then(|at| at.rsplit('/').next())
        .unwrap_or_else(|| panic!("the ask says where the call waits: {asked}"))
        .to_owned();
    let (own, person) = host
        .daemon()
        .own_shell_in(host.workspace())
        .expect("the person's shell reaches the workspace");
    let receipt = own
        .invoke(
            &person,
            host::call(
                tilo_action_services::workspace_invocations::PROVIDER,
                tilo_actions::ActionAddress::new(record, "resolve"),
                &json!({"resolution": "approve"}),
            ),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the person's answer is taken: {error:?}"));
    assert!(
        matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's answer is done: {receipt:?}"
    );
}

/// `tilo_watch` waiting `wait_ms`, as the agent follows who is here and what changed.
pub async fn watch(agent: &host::Agent, arguments: Value) -> (String, bool) {
    said(&agent.tool("tilo_watch", &arguments).await)
}

/// The cursor a `tilo_watch` answer gives, to watch on from what it told.
pub fn cursor(answered: &str) -> String {
    let parsed: Value = serde_json::from_str(answered).unwrap_or(Value::Null);
    parsed["cursor"].as_str().unwrap_or_default().to_owned()
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

/// The commands recorded on the workspace's Terminal session, each its own record as an agent
/// of the person's reads it: listed on the session, then read at the path the session lists it
/// by.
pub async fn terminal_commands(agent: &host::Agent) -> Vec<Value> {
    let listed = agent
        .tool_structured(
            "tilo_ls",
            &json!({"path": "terminal/Terminal", "kind": "resources"}),
        )
        .await;
    assert_ne!(listed["isError"], true, "the session lists: {listed}");
    let mut commands = Vec::new();
    for row in listed["structuredContent"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(path) = row["path"].as_str() else {
            continue;
        };
        commands.push(read_json(agent, path).await);
    }
    commands
}

/// What `tilo_read` of `path` reads, as JSON: the structured answer, which a long read shrinks
/// and keeps whole JSON, where its text is cut where it runs long.
pub async fn read_json(agent: &host::Agent, path: &str) -> Value {
    let read = agent
        .tool_structured("tilo_read", &json!({"path": path}))
        .await;
    assert_ne!(read["isError"], true, "{path} reads: {read}");
    let structured = &read["structuredContent"];
    assert!(structured.is_object(), "{path} reads as JSON: {read}");
    structured.clone()
}

/// Whether a command recorded on the workspace's Terminal session holds `needle`: in its
/// command line, or in what it printed, as an agent of the person's reads the record.
pub async fn terminal_holds(host: &host::Host, needle: &str) -> bool {
    terminal_commands(&host.agent())
        .await
        .iter()
        .any(|command| {
            let mut texts = Vec::new();
            texts_of(command, &mut texts);
            texts.iter().any(|text| text.contains(needle))
        })
}

/// Every text `value` holds.
fn texts_of(value: &Value, texts: &mut Vec<String>) {
    match value {
        Value::String(text) => texts.push(text.clone()),
        Value::Object(fields) => {
            for value in fields.values() {
                texts_of(value, texts);
            }
        }
        Value::Array(values) => {
            for value in values {
                texts_of(value, texts);
            }
        }
        _ => {}
    }
}
