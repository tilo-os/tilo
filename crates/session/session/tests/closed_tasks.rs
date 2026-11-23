//! An agent's step makes a task in Tasks while no Tasks window is open on the person's device:
//! the host runs the package the device installed with no window, the step reaches that instance,
//! and the task is among the app's tasks in the workspace — read there by another device of the
//! person, and shown by the window a person opens afterwards.
//!
//! Nothing opens Tasks first. The step is the agent's own: the workspace's `start` of the closed
//! app runs it without a window and names the instance (`run_windowless`), and Tasks' `new_task`
//! is then delivered to that instance's own published action. The claims are read off the
//! session's own catalogue of the app's resources, off the shell, and off the controls the app's
//! window exposes, never off the step's answer alone.

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

/// The app whose task the step makes.
pub(crate) const APP: &str = "so.tilo.tasks";
/// The kind a task of the app is published under.
const TASK: &str = "tl.so.tilo.tasks.Task";
/// The task the agent's step makes.
const TITLE: &str = "Book the ferry";

/// The titles of the tasks `device` reads in the workspace's Tasks, each once, in the order its
/// session lists them.
pub(crate) fn titles(device: &Device) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for row in device.actions() {
        if row.app_id == APP
            && row.resource.kind == TASK
            && !found.iter().any(|title| title == &row.resource.label)
        {
            found.push(row.resource.label.clone());
        }
    }
    found
}

/// Wait until `device` reads a task titled [`TITLE`].
fn wait_for_the_task(device: &Device) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if titles(device).iter().any(|title| title == TITLE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} reads {:?} instead of a task titled {TITLE:?}\n{}",
            device.label,
            titles(device),
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Tasks' `new_task` as the instance `peer` publishes it: what the agent's step names, once that
/// instance offers it.
fn published_new_task(device: &Device, peer: u64) -> RemoteActionTarget {
    let wanted = peer.to_string();
    let mut listed = None;
    tilo_test_env::wait_until(format!("{} publishes Tasks' new_task", device.label), || {
        listed = device
            .actions()
            .iter()
            .find(|row| {
                row.provider == wanted
                    && row.app_id == APP
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.address.action == "new_task")
            })
            .map(target);
        listed.is_some()
    });
    listed.expect("Tasks' new_task is published by the instance the host started")
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
fn an_agents_step_makes_a_task_with_tasks_closed_and_the_persons_devices_read_it() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    let mut phone = ada.device("Ada's phone");
    for device in [&laptop, &phone] {
        device.install_dev_apps();
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

    // Nothing opened Tasks: the shell shows no window of it, and the app holds no task.
    assert!(
        !shows_window(&laptop, APP),
        "the laptop shows no window of Tasks before the step"
    );
    assert_eq!(
        titles(&laptop),
        Vec::<String>::new(),
        "Tasks holds no task yet"
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

    // Then the app's own `new_task`, delivered to the instance's published action: what the
    // agent's step asks of the app it found running.
    step(
        &laptop,
        &published_new_task(&laptop, peer),
        &json!({"title": TITLE}),
    );
    assert!(
        !shows_window(&laptop, APP),
        "the instance that made the task shows no window"
    );
    wait_for_the_task(&laptop);

    // Another device of the person reads the same task: what the step recorded reaches the phone
    // through the workspace, with no window of Tasks anywhere.
    wait_for_the_task(&phone);
    assert!(
        !shows_window(&phone, APP),
        "the other device shows no window of Tasks"
    );

    // A person who opens Tasks afterwards sees the task listed: the window draws the record the
    // workspace held, and the task is among the resources the window's instance publishes.
    laptop.open(APP);
    laptop.wait_for_window(APP);
    tilo_test_env::wait_until("Tasks' window to publish the task", || {
        laptop.actions().iter().any(|row| {
            row.provider.parse::<u64>().is_ok()
                && serde_json::to_string(row).is_ok_and(|row| row.contains(TITLE))
        })
    });
}
