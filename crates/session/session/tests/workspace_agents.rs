//! The agents a session makes are the workspace's: an agent made from the session — its Settings'
//! Agents tab and a send-to box make one the same way — is written into the workspace's own copy
//! of Chat's record of its agents, which Chat, the search and the person's other devices read, and a
//! session started again finds that one copy.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

tilo_automerge_isolate::test_worker!();

use common::people::{Device, Person, Site};
use serde_json::{Value, json};
use tilo_relay_protocol::workspaces::WorkspaceRole;

/// The names of the agents the workspace's copy of Chat's agents record holds on `device` .
fn agents_held(device: &Device) -> Vec<String> {
    let record = collab_types::workspace_record_id("chat.agent");
    let answer = device.ask(&json!({
        "op": "record_read",
        "app": "so.tilo.chat",
        "records": [record],
        "wait_ms": 0,
    }));
    let mut names = Vec::new();
    agent_names(
        &answer["answer"]["records"][0]["value"]["agents"],
        &mut names,
    );
    names
}

/// The name of every agent `value` holds, however the record keys them.
fn agent_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(fields) if fields.contains_key("keeper") => {
            if let Some(name) = fields.get("name").and_then(Value::as_str) {
                names.push(name.to_owned());
            }
        }
        Value::Object(fields) => fields.values().for_each(|field| agent_names(field, names)),
        Value::Array(items) => items.iter().for_each(|item| agent_names(item, names)),
        _ => {}
    }
}

/// Wait until `device` 's copy of the workspace's agents holds `name` exactly once.
fn wait_for_agent(device: &Device, name: &str) {
    tilo_test_env::wait_until(format!("{} holds the agent {name}", device.label), || {
        agents_held(device)
            .iter()
            .filter(|held| held.as_str() == name)
            .count()
            == 1
    });
}

/// Send `text` to `to` from the session's send-to action, as a window's box does, and answer how
/// it ended.
fn send(device: &Device, to: &Value, text: &str) -> Value {
    let actions = device.actions();
    let row = actions["answer"]["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"].as_str() == Some("session:send-to\u{1f}send"))
        .unwrap_or_else(|| panic!("the session publishes its send: {actions}"))
        .clone();
    device.act_wait(
        row["provider"].as_str().expect("a provider"),
        row["id"].as_str().expect("an id"),
        &json!({ "to": [to], "text": text }),
    )
}

#[test]
fn an_agent_the_session_makes_is_the_workspaces_and_found_by_name() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(WorkspaceRole::Editor);
    site.author_apps(&["so.tilo.chat"]);
    let mut laptop = ada.device("Ada's laptop");
    let member = site.admit(ada.public);
    laptop.install_chat();
    laptop.join(&member);
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);
    laptop.open("so.tilo.chat");
    laptop.wait_for_window("so.tilo.chat");

    // Made as the Agents tab makes one: the session writes it into the workspace's record.
    let made = send(
        &laptop,
        &json!({ "kind": "new-agent", "name": "Scout" }),
        "look around",
    );
    assert!(made.get("err").is_none(), "the agent is made: {made}");
    wait_for_agent(&laptop, "Scout");
    // The workspace's host holds it too: what the person's other devices read.
    site.wait_for_record_of("so.tilo.chat", &laptop);

    // The send-to box finds it by its name alone: the search reads the workspace's record.
    let found = send(
        &laptop,
        &json!({ "kind": "agent", "name": "Scout" }),
        "again",
    );
    assert!(
        found.get("err").is_none(),
        "a box finds the agent by name: {found}"
    );

    // A session started again reads the one copy, and makes no second agent of it.
    laptop.stop();
    laptop.start();
    laptop.wait_holding(&site.workspace);
    wait_for_agent(&laptop, "Scout");
    let again = send(
        &laptop,
        &json!({ "kind": "agent", "name": "Scout" }),
        "still there",
    );
    assert!(
        again.get("err").is_none(),
        "the agent is found after a restart: {again}"
    );
    assert_eq!(
        agents_held(&laptop)
            .iter()
            .filter(|held| held.as_str() == "Scout")
            .count(),
        1,
        "one agent, one copy"
    );
}
