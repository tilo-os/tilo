//! A hosted workspace keeps what it holds when a device of its owner that was never let into it
//! makes a workspace under the hosted one's id. The owner's restored computer, holding none of the
//! workspace's history, asks its shell for that workspace by id: what its host holds stays as it
//! was, and nothing the computer made of its own reaches the host.

#![cfg(target_os = "linux")]

use std::time::Instant;

use serde_json::json;
use tilo_server_protocol::workspaces::Role;

use crate::closed_tasks::{APP, titles, tracker};
use crate::common::people::{Device, Person, Site, at};

/// The issues the tracker holds on `device`, once it reads every one of `wanted`.
fn wait_for_issues(device: &Device, wanted: &[&str]) {
    let deadline = tilo_test_env::hang_deadline();
    let mut after = None;
    loop {
        let (list, revision) = tracker(device, after.as_deref());
        let held = titles(&list);
        if wanted
            .iter()
            .all(|title| held.iter().any(|held| held == title))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} reads the issues {held:?}, not {wanted:?}\n{}",
            device.label,
            device.diagnostic()
        );
        after = Some(revision);
    }
}

/// Make an issue titled `title` on `device`, through the `create` Tasks publishes.
fn make_issue(device: &Device, title: &str) {
    device.open(APP);
    device.wait_for_window(APP);
    let create = device.app_action(APP, "create");
    let made = device.act_confirming(&create, &json!({ "title": title }));
    assert!(
        made.get("err").is_none(),
        "{} made no issue: {made}",
        device.label
    );
}

/// What the site holds of the workspace: its records, and who its governance names in it, the
/// laptop's devices included: they are the same before and after, and no other device was let in.
/// The generation the workspace stands at is left out: every record its people write is a commit
/// that moves it, the issue this test writes next included, and it names nobody.
fn held_at(site: &Site) -> (Vec<(String, String)>, String) {
    let named = site.runtime_summary().map_or_else(
        |error| error,
        |summary| {
            format!(
                "{} is governed by {:?}, whose devices are {:?}",
                summary.name, summary.members, summary.devices
            )
        },
    );
    (site.records(), named)
}

#[test]
fn an_unenrolled_device_of_the_owner_making_the_hosted_workspace_leaves_it_whole() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Acme");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_tasks();
    laptop.join(&site.invite(ada.public, Role::Editor));
    laptop.wait_holding(&site.workspace);
    crate::common::people::show(&laptop, &site);
    make_issue(&laptop, "Book the ferry");
    make_issue(&laptop, "Pack the tent");
    site.wait_for_record_of(APP, &laptop);
    let before = held_at(&site);

    // Restored from the person's words: it holds none of the workspace and no list naming it.
    let mut restored = ada.device("Ada's restored computer");
    restored.install_tasks();
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
    // What the laptop writes next still lands with every issue before it, at the site and on
    // the laptop: the workspace is the one it was.
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
