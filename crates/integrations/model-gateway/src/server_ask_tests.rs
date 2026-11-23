//! An app's turn through the gateway is the person's model on their
//! workspace's server, asked through this computer's daemon
//! ([`tilo_api::calls::ask_model`], whose own tests follow an ask word by
//! word). With no daemon here, an app is told there is no model while offline.
use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;

fn turn(stream: bool) -> Request {
    let tool = agent_provider_config::protocol::ToolDefinition {
        name: "lookup".into(),
        description: "Look something up.".into(),
        input_schema: serde_json::json!({
            "type":"object","properties":{"count":{"type":"integer"}},"required":["count"]
        }),
    };
    Request::CompleteDefault {
        request: agent_provider_config::protocol::Conversation::new("system", "question")
            .request(vec![tool], 64),
        timeout_ms: 5000,
        stream,
    }
}

/// With no daemon on this computer, an app is told there is no model while
/// offline, and nothing else is tried.
async fn offline() {
    let answer = handle(AGENT_APP, turn(false), &provider_client().unwrap()).await;
    assert!(!answer.ok);
    let refused = answer.model_error.expect("a model error");
    assert_eq!(
        refused.kind,
        agent_provider_config::protocol::ErrorKind::Offline
    );
    assert!(
        refused.message.starts_with("No model while offline"),
        "{}",
        refused.message
    );
}

/// Run `name`'s `story` in a process of its own, in a home of its own where no
/// daemon listens and whose computer never logged in.
fn alone(name: &str, marker: &str, story: fn() -> std::pin::Pin<Box<dyn Future<Output = ()>>>) {
    if std::env::var_os(marker).is_some() {
        tilo_rt::block_on(story()).unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_STATE_HOME", root.path())
        .env(marker, "1");
    let output = child.output_locked().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn with_no_server_to_ask_there_is_no_model_while_offline() {
    alone(
        "service::server_ask_tests::with_no_server_to_ask_there_is_no_model_while_offline",
        "GATEWAY_OFFLINE_CHILD",
        || Box::pin(offline()),
    );
}
