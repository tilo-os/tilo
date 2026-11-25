#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! A computer's apps ask the workspace's own model where a page asks it: the ask goes to the
//! model the workspace's person linked, on the device that keeps it, streamed back as the model
//! writes it; with nothing serving that model, the app is told there is no model while offline.

use serde_json::{Value, json};
use tilo_person_services::model_answers::scripted::Reply;

use crate::language::host;

/// One turn asked of the workspace's model, as `account/models#ask` takes it.
fn turn() -> Value {
    json!({
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "when is the launch?"}],
        }],
        "tools": [],
        "tool_choice": "none",
        "max_output_tokens": 256,
    })
}

/// A computer's model asks stream from the device that keeps its workspace: the words come as
/// the model writes them, and the ask reached the model this computer linked.
#[test]
fn a_computers_model_asks_stream_from_its_workspaces_server() {
    host::run(|host| async move {
        // The computer keeps the workspace and answers its turns: the model is linked here, the
        // device that keeps it, and writes its answer in two words.
        let model = host
            .scripted_models([Reply::text("the launch is on Tuesday")
                .pausing(std::time::Duration::from_millis(150))]);
        let workspace = host.workspace().to_owned();
        let key = host.key("Claude Code");
        let stream = key
            .stream(
                "POST",
                &format!("/v1/workspaces/{workspace}/model/ask"),
                &[("content-type", "application/json")],
                turn().to_string().as_bytes(),
            )
            .await;
        let mut seen = String::new();
        let within = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while seen.matches("event: text").count() <= 1 {
            let Ok(Some(chunk)) = tokio::time::timeout_at(within, stream.next()).await else {
                break;
            };
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(
            seen.matches("event: text").count() > 1,
            "the words come as the model writes them: {seen}"
        );
        assert_eq!(
            model.asked().len(),
            1,
            "the computer's own model answered the ask"
        );
    });
}
