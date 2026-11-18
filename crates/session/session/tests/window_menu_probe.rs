//! The window menu is a third party's to drive: a process the session starts reads the menu the
//! session publishes and chooses a row from it, exactly as the desktop's own view does.
//! The probe is the session package's own (`.tests/bin/surface_probe.rs`): it is an ordinary app
//! started the way the session starts a panel, admitted by the launch the session records for it,
//! reading and invoking through the connection the session's broker gives it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it starts is done"
)]

use std::path::Path;
use std::time::Duration;

use crate::common::probe::{probe_binary, reading, until_reported};
use crate::common::tree::{holds, layout_tree};
use serde_json::{Value, json};
use tilo_desktop_model::Layout;
use tilo_session::shell::state::WINDOW_MENU;
use tilo_session::surfaces::Surface;
use tilo_session::{AppSocket, Options, Session};
use tilo_test_env::ScopedEnv;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The session the probe talks to: headless, its app wire on `socket` , showing one window.
fn session_with(socket: &Path, tree: Layout) -> Session {
    let mut session = crate::common::headless(Options {
        collab: false,
        app_socket: AppSocket::At(socket.to_path_buf()),
        ..Options::default()
    });
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    session
}

#[test]
fn a_window_the_session_starts_reads_the_menu_and_chooses_a_row() {
    let root = crate::common::private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut environment = ScopedEnv::new();
    // What the probe is to read and choose, beside what the session writes on its environment when
    // it starts it.
    environment.set("SURFACE_PROBE_REPORT", &report);
    environment.set("SURFACE_PROBE_RESOURCE", WINDOW_MENU);
    environment.set("SURFACE_PROBE_ACTION", "choose");
    environment.set(
        "SURFACE_PROBE_ARGUMENTS",
        json!({ "row": "window-close-window" }).to_string(),
    );
    let mut session = session_with(&socket, layout_tree());
    // The menu is up before the probe starts, so what it reads is the menu a person sees and not an
    // empty shell.
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &json!({ "tab": "20" })
                .as_object()
                .expect("an object")
                .clone(),
        )
        .expect("the menu opens");
    session
        .start_surface(&Surface::new("probe", probe_binary()))
        .expect("the probe starts, as the session starts a panel");

    let report = until_reported(&mut session, &report);

    let first = reading(&report, "read: ");
    assert_eq!(
        first["tab"],
        json!("20"),
        "the menu the session publishes, for the window in front: {report}"
    );
    assert!(
        first["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .filter_map(|row| row["label"].as_str())
            .any(|label| label == "Close this window"),
        "with the rows the shell offers that window: {report}"
    );
    assert!(
        report.lines().any(|line| line == "taken"),
        "the probe's choice is taken by the session: {report}"
    );
    let changed = reading(&report, "changed: ");
    assert_eq!(
        changed["tab"],
        Value::Null,
        "and the menu closes as the reading says: {report}"
    );
    assert!(!holds(&session, 20), "the window it chose to close is gone");
    let published = session.read(WINDOW_MENU).expect("the session publishes it");
    assert_eq!(
        published["tab"],
        Value::Null,
        "the reading a third party drove is the session's own"
    );
}
