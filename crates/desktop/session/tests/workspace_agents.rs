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
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the site hosts the workspace for \
              its whole test"
)]

use crate::common::people::{Device, Person, Site};
use serde_json::{Value, json};
use tilo_server_protocol::workspaces::Role;

/// The names of the agents the workspace's copy of Chat's agents record holds on `device` .
fn agents_held(device: &Device) -> Vec<String> {
    held_agents(device)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// The id and name of each agent the workspace's copy of the agents record holds on `device`.
fn held_agents(device: &Device) -> Vec<(String, String)> {
    let record = collab_types::workspace_document_id("tilo.workspace.agent");
    let answer = in_front(device, ("so.tilo.assistant", &record));
    let mut agents = Vec::new();
    agents_in(&answer["records"][0]["value"]["agents"], &mut agents);
    agents
}

/// The record `route` of the workspace in front on `device`, as its session's record API reads it.
fn in_front(device: &Device, route: (&str, &str)) -> Value {
    let workspace = device.showing().unwrap_or_else(|| {
        panic!(
            "{} shows a workspace: {}",
            device.label,
            device.diagnostic()
        )
    });
    device
        .read(&workspace, route, None, None)
        .unwrap_or_else(|error| {
            panic!(
                "{} reads {}/{} of {workspace}: {error}\n{}",
                device.label,
                route.0,
                route.1,
                device.diagnostic()
            )
        })
}

/// The id and name of every agent `value` holds, however the record keys them.
fn agents_in(value: &Value, agents: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) if fields.contains_key("keeper") => {
            let text = |field: &str| fields.get(field).and_then(Value::as_str).map(str::to_owned);
            if let (Some(id), Some(name)) = (text("id"), text("name")) {
                agents.push((id, name));
            }
        }
        Value::Object(fields) => fields.values().for_each(|field| agents_in(field, agents)),
        Value::Array(items) => items.iter().for_each(|item| agents_in(item, agents)),
        _ => {}
    }
}

/// Wait until Chat's record of the workspace on `device` holds every one of `wanted` : what a
/// send landed as.
fn wait_for_chat_holding(device: &Device, wanted: &[&str]) {
    tilo_test_env::wait_until(Awaited(device, format!("holds {wanted:?} in Chat")), || {
        let held = in_front(device, ("so.tilo.chat", ""))["records"].to_string();
        wanted.iter().all(|needle| held.contains(needle))
    });
}

/// What a wait on `device` waited for, told with what its session warned of once the wait gave up.
struct Awaited<'a>(&'a Device, String);

impl std::fmt::Display for Awaited<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} {}; what its session warned of:\n{}",
            self.0.label,
            self.1,
            self.0.warnings()
        )
    }
}

/// Wait until `device` 's copy of the workspace's agents holds `name` exactly once.
fn wait_for_agent(device: &Device, name: &str) {
    tilo_test_env::wait_until(Awaited(device, format!("holds the agent {name}")), || {
        agents_held(device)
            .iter()
            .filter(|held| held.as_str() == name)
            .count()
            == 1
    });
}

/// Send `text` to `to` (one recipient, or a list of them) from the session's send-to action, as a
/// window's box does, and answer how it ended.
fn send(device: &Device, to: &Value, text: &str) -> Value {
    let to = match to {
        Value::Array(_) => to.clone(),
        one => Value::Array(vec![one.clone()]),
    };
    let send = tilo_test_env::wait_for(
        Awaited(device, "publishes the session's send".into()),
        || device.offered("session:send-to", "send"),
    );
    let answered = device.act_wait(&send, &json!({ "to": to, "text": text }));
    // The send is answered when it is carried out: a receipt left accepted is an invocation
    // nothing settles, which a caller waits out rather than reads.
    let state = answered["update"]["state"].as_str().unwrap_or_default();
    assert!(
        !matches!(state, "accepted" | "progress"),
        "the send settles: {answered}"
    );
    answered
}

/// Ada's laptop in a published workspace, Chat open in it.
fn laptop_with_chat(ada: &Person, site: &Site) -> Device {
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    let member = site.admit(ada.public);
    laptop.join(&member);
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);
    laptop.open("so.tilo.chat");
    laptop.wait_for_window("so.tilo.chat");
    laptop
}

#[test]
fn an_agent_another_device_makes_is_in_the_sessions_copy() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let laptop = laptop_with_chat(&ada, &site);
    let mut phone = ada.device("Ada's phone");
    let member = site.admit(ada.public);
    phone.join(&member);
    phone.wait_holding(&site.workspace);
    phone.wait_showing(&site.workspace);

    // The phone makes an agent in the workspace's record of them, and the laptop's workspace holds
    // it.
    let made = send(
        &phone,
        &json!({ "kind": "new-agent", "name": "Ranger" }),
        "hello",
    );
    assert!(
        made.get("err").is_none(),
        "the phone makes the agent: {made}"
    );
    wait_for_agent(&laptop, "Ranger");

    // The laptop's session took it into its own copy: an agent it makes under the same name is given
    // an id of its own, never the one the phone's agent holds.
    let again = send(
        &laptop,
        &json!({ "kind": "new-agent", "name": "Ranger" }),
        "hello again",
    );
    assert!(again.get("err").is_none(), "the laptop makes one: {again}");
    tilo_test_env::wait_until(
        Awaited(&laptop, "holds both agents called Ranger".into()),
        || {
            held_agents(&laptop)
                .iter()
                .filter(|(_, name)| name == "Ranger")
                .count()
                == 2
        },
    );
    let ids: Vec<String> = held_agents(&laptop)
        .into_iter()
        .filter(|(_, name)| name == "Ranger")
        .map(|(id, _)| id)
        .collect();
    assert_ne!(ids[0], ids[1], "each agent keeps an id of its own: {ids:?}");
}

#[test]
fn a_box_finds_one_agent_of_many_by_name() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let laptop = laptop_with_chat(&ada, &site);

    // Fifty-five agents, made as a box makes them.
    let names: Vec<String> = (1..=55).map(|n| format!("Helper {n:02}")).collect();
    let to: Vec<Value> = names
        .iter()
        .map(|name| json!({ "kind": "new-agent", "name": name }))
        .collect();
    let made = send(&laptop, &Value::Array(to), "start");
    assert!(made.get("err").is_none(), "the agents are made: {made}");
    tilo_test_env::wait_until(Awaited(&laptop, "holds fifty-five agents".into()), || {
        held_agents(&laptop).len() >= 55
    });
    let (id, _) = held_agents(&laptop)
        .into_iter()
        .find(|(_, name)| name == "Helper 42")
        .expect("Helper 42 is made");

    // A box naming one of them alone reaches that one.
    let found = send(
        &laptop,
        &json!({ "kind": "agent", "name": "Helper 42" }),
        "find the forty-second",
    );
    assert!(found.get("err").is_none(), "the send is taken: {found}");
    wait_for_chat_holding(&laptop, &[&id, "find the forty-second"]);
}

#[test]
fn an_agent_the_session_makes_is_the_workspaces_and_found_by_name() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = laptop_with_chat(&ada, &site);

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
