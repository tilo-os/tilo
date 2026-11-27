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

use std::time::{Duration, Instant};

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
    let answer = in_front(device, ("tilo.workspace", &record));
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

/// How long a step of this test waits before it gives up: well inside the runner's own limit, so
/// what the device and its site logged is printed rather than the runner stopping the test as
/// hung first. A step that gave up here says what the device and the apps it runs logged, and the
/// two devices' logs name a clash of the laptop's key where there is one.
const WAIT_LIMIT: Duration = Duration::from_secs(120);

/// Wait until Chat's record of the workspace on `device` holds every one of `wanted` : what a
/// send landed as.
fn wait_for_chat_holding(device: &Device, site: &Site, wanted: &[&str]) {
    wait_until(
        device,
        site,
        WAIT_LIMIT,
        &format!("held {wanted:?} in Chat"),
        || {
            let held = in_front(device, ("so.tilo.chat", ""))["records"].to_string();
            wanted.iter().all(|needle| held.contains(needle))
        },
    );
}

/// Wait until `probe` holds on `device`, giving up after `within` with what the device and its
/// site logged: a key the site saw twice at one sequence number is refused there, and the
/// refusal says so.
fn wait_until(
    device: &Device,
    site: &Site,
    within: Duration,
    what: &str,
    mut probe: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + within;
    loop {
        if probe() {
            return;
        }
        let now = Instant::now();
        assert!(
            now < deadline,
            "{} never {what} within {within:?}; {}\nwhat its site refused:\n{}",
            device.label,
            logs(device),
            site.refusals_report()
        );
        std::thread::sleep(deadline.saturating_duration_since(now).min(POLL));
    }
}

/// Wait until `probe` answers on `device`, giving up after `within` with what the device and its
/// site logged.
fn wait_for<T>(
    device: &Device,
    site: &Site,
    within: Duration,
    what: &str,
    mut probe: impl FnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(answered) = probe() {
            return answered;
        }
        let now = Instant::now();
        assert!(
            now < deadline,
            "{} never {what} within {within:?}; {}\nwhat its site refused:\n{}",
            device.label,
            logs(device),
            site.refusals_report()
        );
        std::thread::sleep(deadline.saturating_duration_since(now).min(POLL));
    }
}

/// How long a wait on `device` pauses between looks.
const POLL: Duration = Duration::from_millis(20);

/// What `device` logged, read from its files: a step that never happened is told with what the
/// device and the daemon it starts said, even where its session no longer answers.
fn logs(device: &Device) -> String {
    let log = device.log();
    let lines: Vec<&str> = log.lines().collect();
    let tail: Vec<&str> = lines.iter().rev().take(80).rev().copied().collect();
    format!(
        "what it warned of:\n{}\nlast lines of its log:\n{}",
        device.warnings(),
        tail.join("\n")
    )
}

/// Wait until `device` 's copy of the workspace's agents holds `name` exactly once.
fn wait_for_agent(device: &Device, site: &Site, name: &str) {
    wait_until(
        device,
        site,
        WAIT_LIMIT,
        &format!("held the agent {name}"),
        || {
            agents_held(device)
                .iter()
                .filter(|held| held.as_str() == name)
                .count()
                == 1
        },
    );
}

/// Send `text` to `to` (one recipient, or a list of them) from the session's send-to action, as a
/// window's box does: the send is carried out, or the test fails here with how it ended.
fn send(device: &Device, site: &Site, to: &Value, text: &str) {
    let to = match to {
        Value::Array(_) => to.clone(),
        one => Value::Array(vec![one.clone()]),
    };
    let send = wait_for(
        device,
        site,
        WAIT_LIMIT,
        "published the session's send",
        || device.offered("session:send-to", "send"),
    );
    let answered = device.act_wait(&send, &json!({ "to": to, "text": text }));
    assert_eq!(
        answered["update"]["state"],
        json!("succeeded"),
        "{} sends {text:?} to {to}: {answered}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// Ada's laptop in a published workspace, Chat open in it.
fn laptop_with_chat(ada: &Person, site: &Site) -> Device {
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    // Chat is a tilolang package: the laptop carries it, as a device with it installed does,
    // before its session starts and reads the apps it has.
    laptop.install_dev_apps();
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
    // The phone sends its message through Chat's own action, so it carries the app too, without
    // a scenario of its own seeded into the workspace.
    phone.install_chat();
    let member = site.admit(ada.public);
    phone.join(&member);
    phone.wait_holding(&site.workspace);
    phone.wait_showing(&site.workspace);
    // Chat's send is the app's own action: the phone opens it, as the laptop's box does.
    phone.open("so.tilo.chat");
    phone.wait_for_window("so.tilo.chat");

    // The phone makes an agent in the workspace's record of them, and the laptop's workspace holds
    // it.
    send(
        &phone,
        &site,
        &json!({ "kind": "new-agent", "name": "Ranger" }),
        "hello",
    );
    wait_for_agent(&laptop, &site, "Ranger");

    // The laptop's session took it into its own copy: an agent it makes under the same name is given
    // an id of its own, never the one the phone's agent holds.
    send(
        &laptop,
        &site,
        &json!({ "kind": "new-agent", "name": "Ranger" }),
        "hello again",
    );
    wait_until(
        &laptop,
        &site,
        WAIT_LIMIT,
        "held both agents called Ranger",
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
    send(&laptop, &site, &Value::Array(to), "start");
    wait_until(&laptop, &site, WAIT_LIMIT, "held fifty-five agents", || {
        held_agents(&laptop).len() >= 55
    });
    let (id, _) = held_agents(&laptop)
        .into_iter()
        .find(|(_, name)| name == "Helper 42")
        .expect("Helper 42 is made");

    // A box naming one of them alone reaches that one.
    send(
        &laptop,
        &site,
        &json!({ "kind": "agent", "name": "Helper 42" }),
        "find the forty-second",
    );
    wait_for_chat_holding(&laptop, &site, &[&id, "find the forty-second"]);
}

#[test]
fn an_agent_the_session_makes_is_the_workspaces_and_found_by_name() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = laptop_with_chat(&ada, &site);

    // Made as the Agents tab makes one: the session writes it into the workspace's record.
    send(
        &laptop,
        &site,
        &json!({ "kind": "new-agent", "name": "Scout" }),
        "look around",
    );
    wait_for_agent(&laptop, &site, "Scout");
    // The workspace's host holds it too: what the person's other devices read.
    wait_until(
        &laptop,
        &site,
        WAIT_LIMIT,
        "the site held Chat's record",
        || site.records().iter().any(|(app, _)| app == "so.tilo.chat"),
    );

    // The send-to box finds it by its name alone: the search reads the workspace's record.
    send(
        &laptop,
        &site,
        &json!({ "kind": "agent", "name": "Scout" }),
        "again",
    );

    // A session started again reads the one copy, and makes no second agent of it.
    laptop.stop();
    laptop.start();
    laptop.wait_holding(&site.workspace);
    wait_for_agent(&laptop, &site, "Scout");
    send(
        &laptop,
        &site,
        &json!({ "kind": "agent", "name": "Scout" }),
        "still there",
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
