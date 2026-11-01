//! The window menu is a third party's to drive: a process the session starts
//! reads the menu the session publishes and chooses a row from it, exactly as
//! the desktop's own view does.
//!
//! The probe is the session package's own (`.tests/bin/surface_probe.rs`): it
//! is an ordinary app started the way the session starts a panel, admitted by
//! the launch the session records for it, reading and invoking through the
//! connection the session's broker gives it.

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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::mount::shell::WINDOW_MENU;
use tilo_session::surfaces::Surface;
use tilo_session::{ActionPlane, Options, Session};
use tilo_test_env::ScopedEnv;

/// How long the probe may take to report what it read.
const REPORTING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
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
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The probe's binary: the session package's own, built beside it.
fn probe_binary() -> PathBuf {
    let binary =
        tilo_test_env::package_binary!("tilo-session").with_file_name("tilo-surface-probe");
    assert!(
        binary.exists(),
        "expected the probe at {}; build the workspace first",
        binary.display()
    );
    binary
}

/// A directory for the plane's socket that only this user reaches: the broker
/// refuses to serve one anybody else could open.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// The session the probe talks to: headless, its plane on `socket`, showing
/// one window.
fn session_with(socket: &Path, tree: DesktopTree) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    session
}

/// The session's turns until the probe has reported.
fn until_reported(session: &mut Session, report: &Path) -> String {
    let deadline = Instant::now() + REPORTING;
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

/// The reading named on the line that starts with `prefix`.
fn reading(report: &str, prefix: &str) -> Value {
    let line = report
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("the probe reported no `{prefix}`: {report}"));
    serde_json::from_str(line).expect("a reading is JSON")
}

/// Whether the desk still holds the window `tab`.
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
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut environment = ScopedEnv::new();
    // What the probe is to read and choose, beside what the session writes on
    // its environment when it starts it.
    environment.set("SURFACE_PROBE_REPORT", &report);
    environment.set("SURFACE_PROBE_RESOURCE", WINDOW_MENU);
    environment.set("SURFACE_PROBE_ACTION", "choose");
    environment.set(
        "SURFACE_PROBE_ARGUMENTS",
        json!({ "row": "window-close-window" }).to_string(),
    );
    let mut session = session_with(&socket, desk_tree());
    // The menu is up before the probe starts, so what it reads is the menu a
    // person sees and not an empty desk.
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
