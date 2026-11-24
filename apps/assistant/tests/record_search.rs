//! What search finds in the workspace's agents, read from the resource itself on
//! a device that never started the Assistant's window.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use tilo_test_env::SpawnUnderEnvLock as _;

use ui::tilo_resource::{NonBlank, Replica};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

/// What the assistant binary answers search about `doc`: its entries, as the
/// session's index takes them.
fn read(doc: &[u8]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_assistant"))
        .arg("--index")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("the assistant binary starts");
    child
        .stdin
        .take()
        .expect("the resource is written to the reader")
        .write_all(doc)
        .expect("the reader takes its resource");
    let out = child.wait_with_output().expect("the reader ends");
    assert!(
        out.status.success(),
        "the resource is read: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("the reader answers entries")
}

#[test]
fn an_agent_is_found_from_the_workspace_resource() {
    let mut agents = Replica::<WorkspaceAgents>::new().as_author("u_ada");
    agents
        .apply(
            WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                avatar: None,
                name: NonBlank::parse("Ada").expect("a name"),
                id: None,
                model: None,
                mcp_client: None,
            }),
        )
        .expect("an agent is made");

    let entries = read(&agents.save());
    let hits: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|entry| entry["title"] == "Ada")
        .collect();
    assert_eq!(hits.len(), 1, "the agent is found by its name");
    assert!(
        hits[0]["resource"]
            .as_str()
            .is_some_and(|resource| resource.contains("/agents/")),
        "a hit opens the agent at its item of the resource: {}",
        hits[0]["resource"]
    );
    assert_eq!(hits[0]["kind"], "agent");
    assert!(
        hits[0]["people"][0]["identity"]
            .as_str()
            .is_some_and(|identity| identity.starts_with("agent:")),
        "an agent's row shows the agent itself: {}",
        hits[0]["people"][0]
    );
    assert!(
        hits[0]["people"][0]["agent"].as_bool().unwrap_or(false),
        "an agent's row shows the agent itself"
    );
}
