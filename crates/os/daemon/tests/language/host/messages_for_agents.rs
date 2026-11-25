//! Messages is an app an agent uses with no window open: the daemon starts its package on the
//! call, the record it keeps holds the conversations delivered into it, and the agent reads,
//! searches, answers and exports them through what the record publishes — none of it asking its
//! person first, and nothing sent from here. The record is read where `tilo_read record:` reads
//! it, through the device's one record reader.

use serde_json::{Value, json};
use tilo_actions::OperationKind;

use super::{Agent, listed, run_with_packages};

const MESSAGES: &str = "so.tilo.messages";
/// The resource every record-level operation is addressed to.
const RECORD: &str = "tilo.messages";
/// The conversation two SMS from one number land in.
const CONVERSATION: &str = "tilo.messages/conversations/+15550100";

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// The typed answer a tool's result carries.
fn typed(result: &Value) -> &Value {
    &result["structuredContent"]
}

/// Run `action` on the record as a written step (`{path: "<app>/<record>#<action>", args}`)
/// does, unconfirmed, and answer with the action's output.
async fn step(agent: &Agent, action: &str, arguments: Value) -> Value {
    let path = format!("{MESSAGES}/{RECORD}#{action}");
    let reply = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": path, "args": arguments, "wait_ms": 30_000}),
        )
        .await;
    let (text, failed) = said(&reply);
    assert!(!failed, "{action}: {text}");
    typed(&reply)["result"].clone()
}

/// What the record holds, as `tilo_read record:<app>:<record>` reads it.
async fn record_of(agent: &Agent) -> Value {
    let reply = agent
        .tool_structured("tilo_read", &json!({"path": format!("record:{MESSAGES}:")}))
        .await;
    let (text, failed) = said(&reply);
    assert!(!failed, "the record reads: {text}");
    typed(&reply)["records"][0]["value"].clone()
}

/// Every message the record holds, across its conversations.
fn messages(record: &Value) -> Vec<Value> {
    record["conversations"]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(_, conversation)| {
            conversation["messages"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, message)| message.clone())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The messages in the order they were written, as their dates say.
fn ordered(record: &Value) -> Vec<Value> {
    let mut messages = messages(record);
    messages.sort_by_key(|message| message["at_ms"].as_i64().unwrap_or_default());
    messages
}

/// The message holding `needle` in its body.
fn holding(record: &Value, needle: &str) -> Value {
    messages(record)
        .into_iter()
        .find(|message| {
            message["body"]
                .as_str()
                .is_some_and(|body| body.contains(needle))
        })
        .unwrap_or_else(|| panic!("a message holds {needle:?}: {record}"))
}

/// Reading the record changes nothing.
async fn unchanged(agent: &Agent) -> Value {
    let first = record_of(agent).await;
    let again = record_of(agent).await;
    assert_eq!(
        first["conversations"], again["conversations"],
        "reading changes nothing"
    );
    first
}

#[test]
fn messages_is_in_the_agents_catalogue() {
    run_with_packages(&[], &[("messages", "messages")], |host| async move {
        let page = listed(&host, MESSAGES)
            .await
            .expect("the catalogue lists the installed app");
        assert!(
            page.rows
                .iter()
                .any(|row| row.app_id == MESSAGES && row.action.is_some()),
            "the agent's catalogue offers Messages: {page:?}"
        );
    });
}

/// One scenario, read top to bottom: an agent delivers two SMS into the record, reads them,
/// searches them, queues a reply and exports the conversation as the content the workspace
/// keeps, and another caller of the person reads the same conversation.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn an_agent_reads_searches_answers_and_exports_sms_delivered_into_a_record() {
    run_with_packages(&[], &[("messages", "messages")], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();

        // Two SMS from one number, delivered the way a harness event delivers them.
        let first = step(
            &agent,
            "deliver-message",
            json!({
                "from": "+1 555 0100",
                "body": "Are we still on for the museum on Saturday?",
                "date": "2026-10-11T09:00:00Z",
            }),
        )
        .await;
        assert_eq!(first["conversation"], CONVERSATION, "{first}");
        step(
            &agent,
            "deliver-message",
            json!({
                "from": "+15550100",
                "to": "+15550199",
                "body": "Tickets are 18 EUR each, I can book two.",
                "date": "2026-10-11T09:05:00Z",
            }),
        )
        .await;

        // What the record publishes for it, as the catalogue lists it.
        let page = listed(&host, MESSAGES)
            .await
            .expect("the catalogue lists what Messages publishes");
        let found = |action: &str| {
            page.rows
                .iter()
                .filter_map(|row| row.action.as_ref())
                .find(|descriptor| {
                    descriptor.address.resource == RECORD && descriptor.address.action == action
                })
                .unwrap_or_else(|| panic!("the record offers {action}"))
                .clone()
        };
        for action in ["deliver-message", "send-message", "reply"] {
            assert_eq!(
                found(action).kind,
                OperationKind::Document,
                "{action} changes the record"
            );
        }
        assert!(
            readable(&found("read")),
            "a planner may read the record while it plans"
        );
        assert!(readable(&found("search-messages")), "and search it");
        assert_eq!(
            found("export-content").kind,
            OperationKind::Effect,
            "an export reaches the content store"
        );

        // Reading: one conversation, both messages, and the second's recipient.
        let held = unchanged(&agent).await;
        assert_eq!(
            held["conversations"]
                .as_object()
                .map_or(0, serde_json::Map::len),
            1,
            "{held}"
        );
        let written = ordered(&held);
        assert_eq!(written.len(), 2, "{held}");
        assert_eq!(
            written[0]["body"], "Are we still on for the museum on Saturday?",
            "{held}"
        );
        assert_eq!(
            written[1]["body"], "Tickets are 18 EUR each, I can book two.",
            "{held}"
        );
        assert_eq!(written[1]["to"], "+15550199", "{held}");
        assert_eq!(written[1]["outgoing"], false, "{held}");

        // Searching.
        let found = step(&agent, "search-messages", json!({"query": "TICKETS eur"})).await;
        assert_eq!(
            found["messages"].as_array().map(Vec::len),
            Some(1),
            "{found}"
        );

        // Replying: queued in the conversation, never sent from here.
        step(
            &agent,
            "reply",
            json!({"conversation": CONVERSATION, "body": "Yes! Please book two."}),
        )
        .await;
        let held = record_of(&agent).await;
        let reply = holding(&held, "Yes! Please book two.");
        assert_eq!(reply["outgoing"], true, "{held}");
        assert_eq!(reply["queued"], true, "{held}");
        assert_eq!(messages(&held).len(), 3, "{held}");

        // Exporting as content the workspace keeps, for a grader or a replay.
        let markdown = step(
            &agent,
            "export-content",
            json!({"format": "md", "conversation": CONVERSATION}),
        )
        .await;
        let content = markdown["content"]
            .as_str()
            .unwrap_or_else(|| panic!("the export names its content: {markdown}"))
            .to_owned();
        let hash = content
            .strip_prefix("tilo.content:")
            .expect("a content reference");
        let key = host.key("Messages export reader");
        let (status, bytes) = key
            .bytes(&format!("/v1/workspaces/{workspace}/contents/{hash}"))
            .await;
        assert_eq!(status, 200, "the workspace keeps the export");
        let markdown = String::from_utf8(bytes).expect("markdown is text");
        for expected in [
            "Are we still on for the museum on Saturday?",
            "Tickets are 18 EUR each",
            "Yes! Please book two.",
        ] {
            assert!(markdown.contains(expected), "{expected}: {markdown}");
        }
        let exported = step(&agent, "export-content", json!({"format": "json"})).await;
        let hash = exported["content"]
            .as_str()
            .expect("the export names its content")
            .strip_prefix("tilo.content:")
            .expect("a content reference");
        let (status, bytes) = key
            .bytes(&format!("/v1/workspaces/{workspace}/contents/{hash}"))
            .await;
        assert_eq!(status, 200);
        let exported: Value = serde_json::from_slice(&bytes).expect("the export is JSON");
        assert_eq!(
            exported["conversations"][0]["messages"]
                .as_array()
                .map(Vec::len),
            Some(3),
            "{exported}"
        );
        assert_eq!(
            messages(&record_of(&agent).await).len(),
            3,
            "an export changes nothing"
        );

        // Another of the person's callers reads the same record: the same three messages.
        let page = host.key("Ada's page");
        let reply = page
            .tool_structured(
                "tilo_read",
                &json!({"workspace": workspace, "path": format!("record:{MESSAGES}:")}),
            )
            .await;
        let (text, failed) = said(&reply);
        assert!(!failed, "another caller reads the record: {text}");
        let shared = typed(&reply)["records"][0]["value"].clone();
        assert_eq!(
            messages(&shared).len(),
            3,
            "the same conversation: {shared}"
        );
        for body in [
            "Are we still on for the museum on Saturday?",
            "Tickets are 18 EUR each",
            "Yes! Please book two.",
        ] {
            let _ = holding(&shared, body);
        }
    });
}

/// A read-only view or effect of the record whose answer a planner can use: what an agent reads
/// through while it plans, without changing anything.
fn readable(descriptor: &tilo_actions::api::ActionDescriptor) -> bool {
    let contract = &descriptor.contract;
    let reads = matches!(descriptor.kind, OperationKind::View | OperationKind::Effect)
        && contract.kind == tilo_action_policy::ActionKind::Read;
    let found = match &contract.result {
        tilo_actions::ResultSpec::Json { .. } => true,
        tilo_actions::ResultSpec::Resource { resource_kind } => {
            resource_kind != tilo_actions::CONTENT_RESOURCE_KIND
        }
        _ => false,
    };
    reads && found && contract.confirmation == tilo_actions::ConfirmationPolicy::None
}
