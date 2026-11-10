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

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_session::shell::state::WINDOW_MENU;
use tilo_session::surfaces::Surface;
use tilo_session::{ActionPlane, Options, Session};
use tilo_test_env::ScopedEnv;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// The desk the session starts on: one window, in front.
fn desk_tree() -> DesktopTree {
    let tabs = vec![window(20, "Notes", "so.tilo.notes")];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The probe's binary: the probe this package's own tests start.
fn probe_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
}

/// The session the probe talks to: headless, its plane on `socket` , showing one window.
fn session_with(socket: &Path, tree: DesktopTree) -> Session {
    let mut session = crate::common::headless(Options {
        collab: false,
        action_plane: ActionPlane::At(socket.to_path_buf()),
        ..Options::default()
    });
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    session
}

/// The session's turns until the probe has reported.
fn until_reported(session: &mut Session, report: &Path) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.settle_services();
        session.step(REFRESH).expect("a turn of the session");
        if let Ok(report) = std::fs::read_to_string(report) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "the probe reported nothing: {:?}",
            session.surfaces().names()
        );
    }
}

/// The reading named on the line that starts with `prefix` .
fn reading(report: &str, prefix: &str) -> Value {
    let line = report
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("the probe reported no `{prefix}`: {report}"));
    serde_json::from_str(line).expect("a reading is JSON")
}

/// Whether the desk still holds the window `tab` .
fn holds(session: &Session, tab: u64) -> bool {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|shown| shown.id == tab)
    })
}

#[test]
fn a_window_the_session_starts_reads_the_menu_and_chooses_a_row() {
    let root = common::private_home();
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
    let mut session = session_with(&socket, desk_tree());
    // The menu is up before the probe starts, so what it reads is the menu a person sees and not an
    // empty desk.
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
        "with the rows the desk offers that window: {report}"
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
