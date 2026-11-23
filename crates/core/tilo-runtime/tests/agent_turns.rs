//! A turn of an agent on the person's daemon is an agent session, as a connected agent's is: what
//! it was asked is the session's purpose, and the work it does is grouped as that one session.

use serde_json::json;

use crate::agent_fixture::{act, events, watch};
use crate::host;

/// The person gives the daemon's agent a turn, asked to plan the launch: what the agent does is
/// one session of its own, named by the agent, its purpose what the person asked, as a connected
/// agent's session reads on the person's page.
#[test]
fn a_turn_on_the_server_is_an_agent_session_the_person_reads() {
    host::run(|host| async move {
        // A turn begins with the client's `initialize`, which names the session (`tilo mcp`
        // writes the harness's name there); the daemon's turns are what answer it.
        let agent = host.agent();
        let opened = agent
            .ask(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"experimental": {"so.tilo": {"session-name": "Plan the launch"}}},
                    "clientInfo": {"name": "claude-code", "version": "2"},
                }),
            )
            .await;
        assert!(
            opened.get("result").is_some(),
            "the turn's initialize: {opened}"
        );
        let (made, failed) = act(&agent, "device#info", json!({})).await;
        assert!(!failed, "the turn's agent works in the workspace: {made}");
        let (watched, failed) = watch(&agent, json!({"wait_ms": 1_000})).await;
        assert!(!failed, "the turn is watched: {watched}");
        let work: Vec<_> = events(&watched)
            .into_iter()
            .filter(|event| event["kind"] == "invocation")
            .collect();
        assert!(!work.is_empty(), "the turn made no work: {watched}");
        assert!(
            work.iter().all(|entry| entry["group"]
                .as_str()
                .is_some_and(|group| group.starts_with("agent-session:"))
                && entry["title"] == "Plan the launch"),
            "every change of the turn is its one session's, for what the person asked: {watched}"
        );
    });
}
