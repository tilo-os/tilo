//! An app the session opens writes its startup timeline in its own state directory, carrying the
//! launcher's marks before its own: an open that took too long is read from it, step by step, from
//! the launcher starting through the sandbox to the app's first frame.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::{Path, PathBuf};

use crate::common::people::Person;

/// The timeline `app` wrote on the device whose app store is `store`: its state directory sits in
/// the directory each workspace gives the app, beside that store.
fn timeline(store: &Path, app: &str) -> Option<PathBuf> {
    let workspaces = store.parent()?.join("tilo-workspaces");
    std::fs::read_dir(workspaces).ok()?.find_map(|workspace| {
        let path = workspace
            .ok()?
            .path()
            .join(app)
            .join(".local/state/startup-timeline.json");
        path.is_file().then_some(path)
    })
}

/// The names a timeline's `key` list holds.
fn names(timeline: &serde_json::Value, key: &str) -> Vec<String> {
    timeline[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mark| mark["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn an_opened_app_writes_its_startup_timeline_with_the_launchers_marks() {
    let _settled = ui::motion::testing::settled();
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_passwords();
    laptop.start();
    tilo_test_env::wait_for("Ada's laptop showing a workspace", || {
        laptop.showing().filter(|showing| !showing.is_empty())
    });

    laptop.open("so.tilo.passwords");
    laptop.wait_for_window("so.tilo.passwords");

    // The app writes its timeline as it goes; its first frame is the last mark of its start.
    let store = laptop.store();
    let written = tilo_test_env::wait_for("Passwords's startup timeline up to its first frame", || {
        let path = timeline(&store, "so.tilo.passwords")?;
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        names(&written, "events")
            .iter()
            .any(|name| name == "first-frame-presented")
            .then_some(written)
    });
    let launcher = names(&written, "launcher");
    for mark in ["launcher-start", "sandbox-entered", "sandbox-exec"] {
        assert!(
            launcher.iter().any(|name| name == mark),
            "the timeline carries the launcher's {mark}: {launcher:?}"
        );
    }
    laptop.stop();
}
