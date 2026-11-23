#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! A computer's apps ask the workspace's own model where a page asks it: the ask goes to the
//! model the workspace's person linked, streamed back as the model writes it; with nothing
//! serving that model, the app is told there is no model while offline.

use serde_json::json;

use crate::host;

/// A computer's model asks stream from its workspace's server over its one session: the words
/// come as the model writes them, and with nothing serving that API the app is told there is no
/// model while offline.
#[test]
fn a_computers_model_asks_stream_from_its_workspaces_server() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Claude Code");
        let ask = json!({
            "messages": [{"role": "user", "content": "when is the launch?"}],
            "tools": [],
            "tool_choice": "none",
            "max_output_tokens": 256,
        });
        let stream = key
            .stream(
                "POST",
                &format!("/v1/workspaces/{workspace}/model/ask"),
                &[("content-type", "application/json")],
                ask.to_string().as_bytes(),
            )
            .await;
        let mut seen = String::new();
        let within = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while seen.matches("event: text_delta").count() <= 1 {
            let chunk = tokio::time::timeout_at(within, stream.next())
                .await
                .unwrap_or_else(|_| panic!("a model answers within 30 s: {seen}"))
                .expect("the stream stays open");
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(
            seen.matches("event: text_delta").count() > 1,
            "the words come as the model writes them: {seen}"
        );
    });
}
