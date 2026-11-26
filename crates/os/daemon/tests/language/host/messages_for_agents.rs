//! Messages is an app an agent uses with no window open: the workspace starts it, the
//! conversations delivered into it are kept in its root, and the agent searches, answers and
//! exports them through the actions the app publishes — none of it asking its person first, and
//! nothing sent from here. An export is content the workspace keeps.

use serde_json::{Value, json};

use super::{Agent, listed, run_with_packages, start_app};

const MESSAGES: &str = "so.tilo.messages";
/// The conversation two SMS from one number land in: the number's digits.
const CONVERSATION: &str = "+15550100";

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

/// Run `action` on Messages' root as the agent's `tilo_do` does, unconfirmed, and answer with
/// the action's typed output.
async fn step(agent: &Agent, action: &str, arguments: Value) -> Value {
    let reply = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": format!("{MESSAGES}#{action}"), "args": arguments, "wait_ms": 30_000}),
        )
        .await;
    let (text, failed) = said(&reply);
    assert!(!failed, "{action}: {text}");
    reply["structuredContent"].clone()
}

/// The `tilo.content:` hash an export's answer names, wherever its typed answer carries it.
fn content_of(answer: &Value) -> String {
    fn find(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => text.strip_prefix("tilo.content:").map(str::to_owned),
            Value::Array(items) => items.iter().find_map(find),
            Value::Object(fields) => fields.values().find_map(find),
            _ => None,
        }
    }
    find(answer).unwrap_or_else(|| panic!("the export names its content: {answer}"))
}

#[test]
fn messages_is_in_the_agents_catalogue() {
    run_with_packages(&["messages"], &[], |host| async move {
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

/// One scenario, read top to bottom: an agent delivers two SMS into Messages, searches them,
/// queues a reply and exports the conversation as content the workspace keeps.
#[test]
fn an_agent_searches_answers_and_exports_sms_delivered_into_messages() {
    run_with_packages(&["messages"], &[], |host| async move {
        start_app(&host, MESSAGES)
            .await
            .unwrap_or_else(|refused| panic!("the workspace starts Messages: {refused:?}"));
        let workspace = host.workspace().to_owned();
        let agent = host.agent();

        // Two SMS from one number, delivered the way a harness event delivers them.
        let first = step(
            &agent,
            "deliver_message",
            json!({
                "from": "+1 555 0100",
                "body": "Are we still on for the museum on Saturday?",
                "date": "2026-10-11T09:00:00Z",
            }),
        )
        .await;
        assert!(first.to_string().contains(CONVERSATION), "{first}");
        step(
            &agent,
            "deliver_message",
            json!({
                "from": "+15550100",
                "to": "+15550199",
                "body": "Tickets are 18 EUR each, I can book two.",
                "date": "2026-10-11T09:05:00Z",
            }),
        )
        .await;

        // Searching: every word, in any case.
        let found = step(&agent, "search_messages", json!({"query": "TICKETS eur"})).await;
        let found = found.to_string();
        assert!(found.contains("Tickets are 18 EUR each"), "{found}");
        assert!(!found.contains("museum"), "{found}");

        // Replying: queued in the conversation, never sent from here.
        step(
            &agent,
            "reply",
            json!({"conversation": CONVERSATION, "body": "Yes! Please book two."}),
        )
        .await;

        // Exporting as content the workspace keeps, for a grader or a replay.
        let key = host.key("Messages export reader");
        let exported = step(
            &agent,
            "export_content",
            json!({"format": "md", "conversation": CONVERSATION}),
        )
        .await;
        let (status, bytes) = key
            .bytes(&format!(
                "/v1/workspaces/{workspace}/contents/{}",
                content_of(&exported)
            ))
            .await;
        assert_eq!(
            status,
            200,
            "the workspace keeps the export {exported}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let markdown = String::from_utf8(bytes).expect("markdown is text");
        for expected in [
            "Are we still on for the museum on Saturday?",
            "Tickets are 18 EUR each",
            "Yes! Please book two.",
        ] {
            assert!(markdown.contains(expected), "{expected}: {markdown}");
        }
        let exported = step(&agent, "export_content", json!({"format": "json"})).await;
        let (status, bytes) = key
            .bytes(&format!(
                "/v1/workspaces/{workspace}/contents/{}",
                content_of(&exported)
            ))
            .await;
        assert_eq!(status, 200);
        let exported = String::from_utf8(bytes).expect("JSON is text");
        assert!(exported.contains("\"queued\":true"), "{exported}");
        assert_eq!(
            exported.matches("\"body\"").count(),
            3,
            "the three messages: {exported}"
        );
    });
}
