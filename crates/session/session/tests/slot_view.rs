//! Which view draws a part of the shell is the person's to choose: binding
//! the window-menu slot to another app's view shows that view where the shell
//! shipped its own, while the menu itself stays the reading it was, and
//! binding it again while the menu is up swaps the view under it.
//!
//! The probe is an ordinary app the session starts for the slot
//! (`.tests/bin/surface_probe.rs`): it reads `desktop:window-menu` through the
//! connection the session's broker admits it for, and reports what it saw.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it starts is done"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::mount::shell::WINDOW_MENU;
use tilo_session::mount::slots::BINDINGS;
use tilo_session::{ActionPlane, Options, Session};
use tilo_settings_service::SettingsService;
use tilo_test_env::{ScopedEnv, TestHome};

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

/// The probe's binary: the probe this package's own tests start.
fn probe_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
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
/// one window, keeping the person's settings as the session does.
fn session_with(socket: &Path, tree: DesktopTree) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            services: vec![Box::new(SettingsService::new())],
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

/// The rows the menu offers, as the reading gives them.
fn rows(session: &Session) -> Value {
    session
        .read(WINDOW_MENU)
        .and_then(|reading| reading.get("rows").cloned())
        .expect("the menu is up and offers its rows")
}

/// Bind the window-menu slot to the view `view` of `app`, as a person or an
/// agent does through the settings door.
fn bind(session: &mut Session, app: &str, view: &str) {
    let mut arguments = Map::new();
    arguments.insert("slot".into(), json!("window-menu"));
    arguments.insert("app".into(), json!(app));
    arguments.insert("view".into(), json!(view));
    session
        .invoke(BINDINGS, "bind", &arguments)
        .expect("the slot takes a binding");
}

/// The program the probe is pointed at, under the name the app `app`'s view
/// runs as: `so.tilo.window-menu` runs as `tilo-window-menu`
/// ([`Binding::program`](tilo_session::mount::slots::Binding::program)).
fn installed(environment: &mut ScopedEnv, app: &str, probe: &Path) {
    let program = format!(
        "tilo-{}",
        app.strip_prefix("so.tilo.")
            .unwrap_or(app)
            .replace('.', "-")
    );
    environment.set(
        format!("TILO_SURFACE_{}", program.to_uppercase().replace('-', "_")),
        probe,
    );
}

#[test]
fn a_bound_view_draws_the_menu_and_binding_another_swaps_it_while_it_is_up() {
    let _home = TestHome::new().expect("a home of the test's own");
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut environment = ScopedEnv::new();
    let probe = probe_binary();
    // The same probe under two names: what the session starts for the slot
    // when the person binds one app's view, and another.
    installed(&mut environment, "so.tilo.probe-one", &probe);
    installed(&mut environment, "so.tilo.probe-two", &probe);
    environment.set("SURFACE_PROBE_REPORT", &report);
    environment.set("SURFACE_PROBE_RESOURCE", WINDOW_MENU);
    let mut session = session_with(&socket, desk_tree());
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
    let offered = rows(&session);
    let before = session.read(WINDOW_MENU).expect("the menu is up");

    // The person binds the slot while the menu is up: the view they chose
    // draws it, and it reads the menu the shell published.
    bind(&mut session, "so.tilo.probe-one", "menu");
    let first = until_reported(&mut session, &report);
    let seen = reading(&first, "read: ");
    assert_eq!(
        seen["rows"], offered,
        "the view bound to the slot shows the rows the menu offers: {first}"
    );
    assert_eq!(
        session.read(WINDOW_MENU).expect("the reading"),
        before,
        "and the menu itself is the same reading it was"
    );
    assert!(
        session.surfaces().running("window-menu"),
        "the view the person bound is what draws the slot"
    );

    // Another binding, while the menu is still up: the view that drew it goes
    // and the one bound now starts, over the same reading.
    std::fs::remove_file(&report).expect("the report is the first view's");
    bind(&mut session, "so.tilo.probe-two", "menu");
    let second = until_reported(&mut session, &report);
    let seen = reading(&second, "read: ");
    assert_eq!(
        seen["rows"], offered,
        "the view bound now shows the same rows: {second}"
    );
    assert_eq!(
        session.read(WINDOW_MENU).expect("the reading"),
        before,
        "rebinding the slot never touched the menu the shell publishes"
    );
}

#[test]
fn a_part_the_system_draws_refuses_a_binding() {
    let _home = TestHome::new().expect("a home of the test's own");
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let mut session = session_with(&socket, desk_tree());
    for slot in ["lock", "consent", "portal", "auth", "display-confirm"] {
        let mut arguments = Map::new();
        arguments.insert("slot".into(), json!(slot));
        arguments.insert("app".into(), json!("so.tilo.probe-one"));
        arguments.insert("view".into(), json!("menu"));
        let refused = session
            .invoke(BINDINGS, "bind", &arguments)
            .expect_err("the system's own part refuses a view of someone else's");
        assert!(
            format!("{refused}").contains("draws"),
            "the refusal says the shell draws it itself: {refused}"
        );
    }
    let bound = session
        .read(BINDINGS)
        .and_then(|reading| reading.get("bindings").cloned())
        .expect("the settings read");
    assert_eq!(bound, json!({}), "and nothing was bound");
}
