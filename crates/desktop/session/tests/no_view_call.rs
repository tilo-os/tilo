//! An agent's call of a tilolang app shows nothing: the workspace's call of the app reaches its
//! process start, never a window. Nothing opens Tasks first, and no view of it appears.
//!
//! The call is answered by what the process start says. A machine with the app's runtimes
//! installed answers it from the app; this session check installs none, so the answer says that,
//! and what it never does is open a window instead. (What the app does with the call is proved
//! in `tilolang-process`'s suite, where a process runs in the test itself.)

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the device lives until its session ends"
)]

use serde_json::json;
use tilo_server_protocol::workspaces::Role;

use crate::common::people::{Device, Person, Site, at, join_together};
use tilo_actions::remote::RemoteActionTarget;

/// The app whose code the agent's call runs.
const APP: &str = "so.tilo.tasks";
/// The workspace's `start` of [`APP`], as the host names it.
fn start_action(workspace: &str) -> RemoteActionTarget {
    at(
        &tilo_actions::remote::workspace_provider(workspace),
        &format!("{}{APP}", tilo_actions::remote::APPLICATION_RESOURCE_PREFIX),
        tilo_actions::remote::APPLICATION_START_ACTION,
    )
}

/// Whether `device`'s shell shows a view of `app`.
fn shows_window(device: &Device, app: &str) -> bool {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["app_id"].as_str() == Some(app)))
}

#[test]
fn a_call_of_a_tilolang_app_with_nothing_shown_never_opens_a_view() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    // The device carries the apps a development session installs, as `tilolang run` leaves them.
    let mut laptop = ada.device("Ada's laptop");
    crate::common::dev_apps::install(&laptop.store(), &laptop.store().join("scratch"));
    let hers = site.admit(ada.public);
    join_together(&mut [&mut laptop], &[hers.as_str()]);
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);

    // Nothing opened Tasks: the shell shows no view of it, and its process offers nothing.
    assert!(
        !shows_window(&laptop, APP),
        "no view of Tasks before the call"
    );

    // The call an agent makes: the workspace's `start` of the app. It reaches the app's process
    // start, which needs runtimes this check installs none of, and it is answered with that.
    let answer = laptop.act_wait(&start_action(&site.workspace), &json!({}));
    assert!(
        answer
            .get("err")
            .is_some_and(|error| error.to_string().contains("could not run without a window")),
        "the call is answered by the process start: {answer}"
    );
    assert!(
        !shows_window(&laptop, APP),
        "the call opened no view: {:?}",
        laptop.ask(&json!({"op": "windows"}))
    );
    // Nothing of the app is running, so no call of it is offered either: a view would have to
    // open for one, and none did.
    let published: Vec<String> = laptop
        .actions()
        .into_iter()
        .filter(|row| row.app_id == APP)
        .map(|row| {
            format!(
                "{} {} {:?}",
                row.resource.kind, row.resource.id, row.resource.label
            )
        })
        .collect();
    assert!(
        published.is_empty(),
        "no process of the app publishes anything here: {published:?}"
    );
}
