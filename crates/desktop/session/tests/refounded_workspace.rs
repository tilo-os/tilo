//! A hosted workspace keeps what it holds when a device of its owner that was never let into it
//! makes a workspace under the hosted one's id. The owner's restored computer, holding none of the
//! workspace's history, asks its shell for that workspace by id: what its host holds stays as it
//! was, and nothing the computer made of its own reaches the host.

#![cfg(target_os = "linux")]

use std::time::Instant;

use serde_json::json;
use tilo_server_protocol::workspaces::Role;

use crate::closed_tasks::{APP, titles};
use crate::common::people::{Device, Person, Site, at};

/// The tasks the workspace's Tasks holds on `device`, once it reads every one of `wanted`.
fn wait_for_issues(device: &Device, wanted: &[&str]) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = titles(device);
        if wanted
            .iter()
            .all(|title| held.iter().any(|held| held == title))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} reads the tasks {held:?}, not {wanted:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Make a task titled `title` on `device`, through the `new_task` Tasks publishes.
fn make_issue(device: &Device, title: &str) {
    device.open(APP);
    device.wait_for_window(APP);
    let made = device.app_act(APP, "new_task", &json!({ "title": title }));
    assert!(
        made.get("err").is_none(),
        "{} made no task: {made}",
        device.label
    );
}

/// What the site holds of the workspace: its records, and who its governance names in it, the
/// laptop's devices included: they are the same before and after, and no other device was let in.
/// The generation the workspace stands at is left out: every record its people write is a commit
/// that moves it, the task this test writes next included, and it names nobody.
fn held_at(site: &Site) -> (Vec<(String, String)>, String) {
    let named = site.runtime_summary().map_or_else(
        |error| error,
        |summary| {
            format!(
                "{} is hosted by {:?}, whose devices are {:?}",
                summary.name, summary.members, summary.devices
            )
        },
    );
    (site.records(), named)
}

#[test]
fn a_device_the_owner_has_not_added_making_the_workspace_on_a_server_leaves_it_whole() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Acme");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_dev_apps();
    laptop.join(&site.invite(ada.public, Role::Editor));
    laptop.wait_holding(&site.workspace);
    crate::common::people::show(&laptop, &site);
    make_issue(&laptop, "Book the ferry");
    make_issue(&laptop, "Pack the tent");
    site.wait_for_record_of(APP, &laptop);
    let before = held_at(&site);

    // Another computer of hers, just let in: it holds none of the workspace.
    let mut restored = ada.device("Ada's restored computer");
    restored.install_dev_apps();
    restored.start();
    tilo_test_env::wait_for("the restored computer showing a workspace", || {
        restored.showing().filter(|shown| !shown.is_empty())
    });
    let asked = restored.act_wait(
        &at("so.tilo.session", "desktop:chrome", "new-workspace"),
        &json!({"workspace": site.workspace, "name": "Acme"}),
    );
    assert!(asked.get("err").is_none(), "the shell answers: {asked}");
    // The shell puts no workspace of its own under the hosted one's id: one this computer holds
    // nothing of is entered or joined first.
    assert!(
        !restored.on_shell().contains(&site.workspace),
        "the restored computer shows no copy of its own of {}: {:?}\n{}",
        site.workspace,
        restored.on_shell(),
        restored.log()
    );
    assert!(
        !restored.held().contains(&site.workspace),
        "the restored computer founded nothing under the workspace's id\n{}",
        restored.log()
    );
    // What the laptop writes next still lands with every task before it, at the site and on the
    // laptop: the workspace is the one it was.
    make_issue(&laptop, "Buy the tickets");
    wait_for_issues(
        &laptop,
        &["Book the ferry", "Pack the tent", "Buy the tickets"],
    );
    let after = held_at(&site);
    for record in &before.0 {
        assert!(
            after.0.contains(record),
            "the site still holds {record:?}: before {before:?}, after {after:?}"
        );
    }
    assert_eq!(
        after.1, before.1,
        "the site's governance names who it named"
    );
    for device in [&mut laptop, &mut restored] {
        device.stop();
    }
}
