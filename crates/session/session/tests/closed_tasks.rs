//! An agent's step makes an issue in Tasks while no Tasks window is open on the person's device:
//! the host runs the package the device installed with no window, the step reaches that instance,
//! and the issue is among the tracker's issues in the workspace — read there by another device of
//! the person, and shown by the window a person opens afterwards.
//!
//! Nothing opens Tasks first. The step is the agent's own: the workspace's `start` of the closed
//! app runs it without a window and names the instance (`run_windowless`), and the tracker's
//! `create` is then delivered to that instance's own published action. The claims are read back
//! off the workspace's records, off the shell, and off the controls the app's window exposes, never
//! off the step's answer alone.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use std::time::Instant;

use crate::common::people::{
    Device, Person, Site, join_together, shows_window, start_action, target,
};
use serde_json::{Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_server_protocol::workspaces::Role;

/// The app whose record the step changes.
pub(crate) const APP: &str = "so.tilo.tasks";
/// The issue the agent's step makes.
const TITLE: &str = "Book the ferry";

/// The first part of the tracker's issues, kept beside its record: where a new issue goes.
fn first_part() -> String {
    ui::resource::DocumentId::derived("issues", &ui::resource::DocumentId::workspace_default())
        .part(0)
        .to_string()
}

/// The first part of the tracker's issues as `device` reads it now, or once it differs from
/// `after`: its value, and the revision an answer names so the next read can wait on the next
/// write.
pub(crate) fn tracker(device: &Device, after: Option<&str>) -> (Value, String) {
    let deadline = tilo_test_env::hang_deadline();
    let wait = deadline.saturating_duration_since(Instant::now());
    let answer = device.ask(&json!({
        "op": "resource_read",
        "app": APP,
        "records": [first_part()],
        "after": after,
        "wait_ms": u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
    }));
    assert!(
        answer.get("err").is_none(),
        "{} reads the tracker: {answer}",
        device.label
    );
    let answer = &answer["answer"];
    let record = answer["records"]
        .as_array()
        .and_then(|records| records.first())
        .map_or(Value::Null, |record| record["value"].clone());
    (
        record,
        answer["revision"].as_str().unwrap_or_default().to_owned(),
    )
}

/// The titles of the issues a part of the tracker holds.
pub(crate) fn titles(list: &Value) -> Vec<String> {
    list["issues"]
        .as_object()
        .map(|issues| {
            issues
                .values()
                .filter_map(|issue| {
                    let issue = issue.get("v").unwrap_or(issue);
                    issue["title"].as_str().map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until `device` reads an issue titled [`TITLE`] in the workspace's tracker, and answer the
/// record that holds it: what its session holds of the record, woken by each write to it.
fn wait_for_the_issue(device: &Device) -> Value {
    let mut after = None;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (record, revision) = tracker(device, after.as_deref());
        if titles(&record).iter().any(|title| title == TITLE) {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "{} reads {:?} instead of an issue titled {TITLE:?}\n{}",
            device.label,
            titles(&record),
            device.diagnostic()
        );
        after = Some(revision);
    }
}

/// The tracker's `create` as the instance `peer` publishes it: what the agent's step names, once
/// that instance offers it.
fn published_create(device: &Device, peer: u64) -> RemoteActionTarget {
    let wanted = peer.to_string();
    let mut listed = None;
    tilo_test_env::wait_until(format!("{} publishes Tasks' create", device.label), || {
        listed = device
            .actions()
            .iter()
            .find(|row| {
                row.provider == wanted
                    && row.app_id == APP
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.address.action == "create")
            })
            .map(target);
        listed.is_some()
    });
    listed.expect("the tracker's create is published by the instance the host started")
}

/// Invoke `action` on `device` with `args` and wait for what it answered.
fn step(device: &Device, action: &RemoteActionTarget, args: &Value) -> Value {
    let id = &action.address;
    let answer = device.act_wait(action, args);
    match answer["update"]["state"].as_str() {
        Some("succeeded") => answer["update"]["output"]["value"].clone(),
        Some("failed") => panic!(
            "{} {id:?} failed: {}\n{answer}\n{}",
            device.label,
            answer["update"]["output"]["error"],
            device.diagnostic()
        ),
        other => panic!(
            "{} {id:?} ended as {other:?}: {answer}\n{}",
            device.label,
            device.diagnostic()
        ),
    }
}

#[test]
fn an_agents_step_makes_an_issue_with_tasks_closed_and_the_persons_devices_read_it() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    // The workspace lets Tasks write the tracker its people read there, as its host does before
    // they open the app in it.
    let mut laptop = ada.device("Ada's laptop");
    let mut phone = ada.device("Ada's phone");
    for device in [&laptop, &phone] {
        device.install_tasks();
    }
    // The workspace's entry admits both, and neither joins through the other: both sessions boot
    // before either is waited for.
    let hers = site.admit(ada.public);
    let her_phones = site.admit(ada.public);
    join_together(
        &mut [&mut laptop, &mut phone],
        &[hers.as_str(), her_phones.as_str()],
    );
    for device in [&laptop, &phone] {
        device.wait_holding(&site.workspace);
        // The shell shows the workspace: what the API acts in.
        device.wait_showing(&site.workspace);
    }

    // Nothing opened Tasks: the shell shows no window of it, and the tracker holds no issue.
    assert!(
        !shows_window(&laptop, APP),
        "the laptop shows no window of Tasks before the step"
    );
    let (before, _) = tracker(&laptop, None);
    assert_eq!(
        titles(&before),
        Vec::<String>::new(),
        "the tracker holds no issue yet"
    );

    // The step an agent takes: the workspace's `start` of the closed app, which runs it with no
    // window and names the instance.
    let started = step(&laptop, &start_action(&site.workspace, APP), &json!({}));
    let peer = started["peer_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("the start names the instance it ran: {started}"));
    assert!(
        !shows_window(&laptop, APP),
        "the start ran Tasks without a window: {:?}",
        laptop.ask(&json!({"op": "windows"}))
    );

    // Then the tracker's own `create`, delivered to the instance's published action: what the
    // agent's step asks of the app it found running. It answers when the record holds the issue.
    step(
        &laptop,
        &published_create(&laptop, peer),
        &json!({"title": TITLE}),
    );
    assert!(
        !shows_window(&laptop, APP),
        "the instance that made the issue shows no window"
    );
    let record = wait_for_the_issue(&laptop);
    assert!(
        titles(&record).iter().any(|title| title == TITLE),
        "the issue is in the workspace's tracker"
    );

    // Another device of the person holds the same record: what the step recorded reaches the
    // phone through the workspace, with no window of Tasks anywhere.
    let elsewhere = wait_for_the_issue(&phone);
    assert!(
        titles(&elsewhere).iter().any(|title| title == TITLE),
        "the person's other device reads the issue"
    );
    assert!(
        !shows_window(&phone, APP),
        "the other device shows no window of Tasks"
    );

    // A person who opens Tasks afterwards sees the issue listed: the window draws the record the
    // workspace held, and the issue is among the resources the window's instance publishes.
    laptop.open(APP);
    laptop.wait_for_window(APP);
    tilo_test_env::wait_until("Tasks' window to publish the issue", || {
        laptop.actions().iter().any(|row| {
            row.provider.parse::<u64>().is_ok()
                && serde_json::to_string(row).is_ok_and(|row| row.contains(TITLE))
        })
    });
}
