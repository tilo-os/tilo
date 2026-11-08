//! Which view draws a part of the shell is the person's to choose: binding the window-menu slot to
//! another app's view shows that view where the shell shipped its own, while the menu itself stays
//! the reading it was, and binding it again while the menu is up swaps the view under it.
//! The probe is an ordinary app the session starts for the slot (`.tests/bin/surface_probe.rs`): it
//! reads `desktop:window-menu` through the connection the session's broker admits it for, and
//! reports what it saw.
//! A view written in tilolang is not a program the session starts: the session runs it itself, over
//! the same reading, and its presses reach the menu's own verbs through the session's door. The
//! view here is compiled against the surface the session publishes for the slot, as a person
//! writing one does.

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

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_compositor::InputEvent;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_session::mount::shell::WINDOW_MENU;
use tilo_session::mount::slots::BINDINGS;
use tilo_session::{ActionPlane, Options, Session};
use tilo_settings_service::SettingsService;
use tilo_test_env::{ScopedEnv, TestHome};

/// How long the probe may take to report what it read. One display's refresh: how often the session
/// asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The pointer's primary button, as the compositor names it.
const BTN_LEFT: u32 = 0x110;

/// A view of the window menu written in tilolang, over the surface the session publishes for the
/// slot: it says whether the menu is up, which it reads from the menu, and its buttons invoke the
/// menu's own verbs — one that moves the menu and leaves it up, one that closes it.
const MENU_VIEW: &str = r#"app so.tilo.slotprobe "Window menu probe"

use desktop as desktop

view menu(m: desktop.WindowMenu) {
  stack {
    text(if m.tab != .none { "A window's menu" } else { "No window's menu" })
    button("Move the menu", on: desktop.open(m, tab: .none, anchor: .some("40,50"))) as moved
    button("Close the menu", on: desktop.close(m)) as close
  }
}
"#;

/// A view of the window menu written in tilolang that shows the rows the menu offers, each a button
/// invoking the menu's `choose` , over the surface the session publishes for the slot. A rule
/// between groups is a line, not a choice, so it draws no button; the rows of a row that opens a
/// submenu are drawn beside it, as the menu itself offers them.
const ROWS_VIEW: &str = r#"app so.tilo.slotrows "Window menu rows"

use desktop as desktop

view menu(m: desktop.WindowMenu) {
  stack {
    for item in m.rows {
      if not item.separator {
        button(item.label, on: desktop.choose(m, row: item.id))
        for child in item.children {
          button(child.label, on: desktop.choose(m, row: child.id))
        }
      }
    }
  }
}
"#;

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

/// The session the probe talks to: headless, its plane on `socket` , showing one window, keeping
/// the person's settings as the session does.
fn session_with(socket: &Path, tree: DesktopTree) -> Session {
    let mut session = crate::common::headless(Options {
        collab: false,
        action_plane: ActionPlane::At(socket.to_path_buf()),
        services: vec![Box::new(SettingsService::new())],
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

/// The rows the menu offers, as the reading gives them.
fn rows(session: &Session) -> Value {
    session
        .read(WINDOW_MENU)
        .and_then(|reading| reading.get("rows").cloned())
        .expect("the menu is up and offers its rows")
}

/// Whether the view draws a control for every row `rows` offers, the rows of a submenu with them: a
/// row that opens one is a row too, and the rule between two groups is no choice to draw.
fn draws_every_row(session: &Session, rows: &Value) -> bool {
    rows.as_array().into_iter().flatten().all(|row| {
        let drawn = row["separator"].as_bool() == Some(true)
            || row["label"]
                .as_str()
                .is_some_and(|label| session.slot_control("window-menu", label).is_some());
        drawn && draws_every_row(session, &row["children"])
    })
}

/// The label of the row `id` offers, in a submenu or not.
fn row_label(offered: &Value, id: &str) -> String {
    fn search<'a>(rows: &'a Value, id: &str) -> Option<&'a Value> {
        rows.as_array().into_iter().flatten().find_map(|row| {
            if row["id"] == json!(id) {
                Some(row)
            } else {
                search(&row["children"], id)
            }
        })
    }
    search(offered, id)
        .and_then(|row| row["label"].as_str())
        .unwrap_or_else(|| panic!("the menu offers `{id}`: {offered}"))
        .to_owned()
}

/// Bind the window-menu slot to the view `view` of `app` , as a person or an agent does through the
/// settings door.
fn bind(session: &mut Session, app: &str, view: &str) {
    let mut arguments = Map::new();
    arguments.insert("slot".into(), json!("window-menu"));
    arguments.insert("app".into(), json!(app));
    arguments.insert("view".into(), json!(view));
    session
        .invoke(BINDINGS, "bind", &arguments)
        .expect("the slot takes a binding");
}

/// The program the probe is pointed at, under the name the app `app` 's view runs as:
/// `so.tilo.window-menu` runs as `tilo-window-menu`
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

/// The session's turns until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.settle_services();
        session.step(REFRESH).expect("a turn of the session");
        if done(session) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the session never {what}: {:?}",
            session.slot_labels("window-menu")
        );
    }
}

/// Press the control the slot's view shows as `name` with the pointer, as a person does.
#[track_caller]
fn press(session: &mut Session, name: &str) {
    let (x, y, w, h) = session
        .slot_control("window-menu", name)
        .unwrap_or_else(|| {
            panic!(
                "the slot's view shows {name}: {:?}",
                session.slot_labels("window-menu")
            )
        });
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer moves");
    session.step(REFRESH).expect("a turn of the session");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .expect("the pointer presses");
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// The package `source` compiles to, against the surface the session publishes for `slot` and
/// written where the session finds a package of this machine: a payload directory holding
/// `app.wasm` , as an installed app's has.
#[track_caller]
fn package(dir: &Path, slot: &str, source: &str) -> PathBuf {
    let surface =
        tilo_session::mount::slots::surface(slot).expect("the slot's state has a surface");
    let surfaces = tilolang_tilo::catalogue::parse_surfaces(&surface.text)
        .expect("the surface the session publishes for the slot");
    let meta = tilolang_tilo::catalogue::package_of(&surface.app, &surface.title, &surfaces);
    let compiled = tilolang_tilo::tilolang::compile_package(
        source,
        &tilolang_tilo::tilolang::IdsLock::new(),
        std::slice::from_ref(&meta),
    )
    .unwrap_or_else(|diagnostics| panic!("the view does not compile: {diagnostics:#?}"));
    std::fs::create_dir_all(dir).expect("the package's directory");
    std::fs::write(dir.join("app.wasm"), &compiled.wasm).expect("the package's module");
    std::fs::write(dir.join("app.ids"), compiled.ids.to_text()).expect("the package's ids");
    dir.to_path_buf()
}

#[test]
fn a_bound_view_draws_the_menu_and_binding_another_swaps_it_while_it_is_up() {
    let _home = TestHome::new().expect("a home of the test's own");
    let root = common::private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut environment = ScopedEnv::new();
    let probe = probe_binary();
    // The same probe under two names: what the session starts for the slot when the person binds
    // one app's view, and another.
    installed(&mut environment, "so.tilo.probe-one", &probe);
    installed(&mut environment, "so.tilo.probe-two", &probe);
    environment.set("SURFACE_PROBE_REPORT", &report);
    environment.set("SURFACE_PROBE_RESOURCE", WINDOW_MENU);
    // The probe is the view that draws the slot here: it stays up while the menu is, as a view
    // does, so the session keeps it as the slot's.
    environment.set("SURFACE_PROBE_STAY", "1");
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

    // The person binds the slot while the menu is up: the view they chose draws it, and it reads
    // the menu the shell published.
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

    // Another binding, while the menu is still up: the view that drew it goes and the one bound now
    // starts, over the same reading.
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
    let root = common::private_home();
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

#[test]
fn a_package_bound_to_the_slot_reads_the_menu_and_closes_it_through_its_own_verb() {
    let _settled = ui::motion::testing::settled();
    let _home = TestHome::new().expect("a home of the test's own");
    let root = common::private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let payload = package(&root.path().join("slot-probe"), "window-menu", MENU_VIEW);
    let mut environment = ScopedEnv::new();
    // The view the person binds, and the one the session ships with while they have bound none.
    installed(&mut environment, "so.tilo.slotprobe", &payload);
    installed(&mut environment, "so.tilo.window-menu", &probe_binary());
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
    until(&mut session, "draws the menu", |session| {
        session.surfaces().running("window-menu")
    });

    // The person binds a view written in tilolang: the session runs it itself, over the reading it
    // publishes for the slot, and the program that drew the menu goes.
    bind(&mut session, "so.tilo.slotprobe", "menu");
    until(&mut session, "draws the view the person bound", |session| {
        session
            .slot_control("window-menu", "Close the menu")
            .is_some()
    });
    assert!(
        !session.surfaces().running("window-menu"),
        "the session runs the package in its own process, not as a program"
    );
    let labels = session.slot_labels("window-menu");
    assert!(
        labels.iter().any(|label| label == "A window's menu"),
        "the view reads the menu the session publishes, which has a window's menu up: {labels:?}"
    );
    assert_eq!(
        session
            .read(WINDOW_MENU)
            .and_then(|reading| reading.get("tab").cloned()),
        Some(json!("20")),
        "and reading it changed nothing"
    );

    // The person presses what the view drew: the menu moves where the view's own argument says,
    // which no press of the session's own could do, so what ran is the verb the view asked for,
    // through the session's door.
    press(&mut session, "Move the menu");
    until(
        &mut session,
        "moves the menu where the view asked it to",
        |session| {
            session.read(WINDOW_MENU).is_some_and(|reading| {
                reading.get("anchor") == Some(&json!({ "x": 40.0, "y": 50.0 }))
            })
        },
    );
    assert_eq!(
        session
            .read(WINDOW_MENU)
            .and_then(|reading| reading.get("tab").cloned()),
        Some(json!("20")),
        "and the menu is still the menu it was, over the window it was for"
    );
    assert!(
        session
            .slot_control("window-menu", "Close the menu")
            .is_some(),
        "the view stands where the reading puts it, with the menu still up"
    );

    // And pressing the button that closes the menu closes it.
    press(&mut session, "Close the menu");
    until(
        &mut session,
        "closes the menu the view asked it to",
        |session| {
            session
                .read(WINDOW_MENU)
                .is_some_and(|reading| reading.get("tab").is_some_and(Value::is_null))
        },
    );
    let namespace = tilo_session::surfaces::namespace(tilo_session::mount::shell::WINDOW_MENU_SLOT);
    assert_eq!(
        session.compositor().layer_shown(&namespace),
        Some(false),
        "and the view it drew stands hidden until a menu opens again"
    );
}

#[test]
fn a_package_that_is_no_view_of_the_slot_leaves_it_to_the_one_the_session_ships_with() {
    let _settled = ui::motion::testing::settled();
    let _home = TestHome::new().expect("a home of the test's own");
    let root = common::private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let payload = package(&root.path().join("slot-probe"), "window-menu", MENU_VIEW);
    let mut environment = ScopedEnv::new();
    installed(&mut environment, "so.tilo.slotprobe", &payload);
    installed(&mut environment, "so.tilo.window-menu", &probe_binary());
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
    until_reported(&mut session, &report);

    // The package the person bound declares no view by that name: it is no view for the slot, and
    // the session puts its own back rather than leaving the menu undrawn.
    std::fs::remove_file(&report).expect("the shipped view's report");
    bind(&mut session, "so.tilo.slotprobe", "no-such-view");
    let again = until_reported(&mut session, &report);
    assert!(
        again.starts_with("read: "),
        "the view the session ships with draws the menu again: {again}"
    );
    assert!(
        session
            .slot_control("window-menu", "Close the menu")
            .is_none(),
        "and the package that did not run draws nothing"
    );
    assert_eq!(
        session
            .read(WINDOW_MENU)
            .and_then(|reading| reading.get("tab").cloned()),
        Some(json!("20")),
        "the menu is still the menu the person opened"
    );
}

#[test]
fn a_package_bound_to_the_slot_shows_the_menu_rows_and_choosing_one_runs_it() {
    let _settled = ui::motion::testing::settled();
    let _home = TestHome::new().expect("a home of the test's own");
    let root = common::private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let payload = package(&root.path().join("slot-rows"), "window-menu", ROWS_VIEW);
    let mut environment = ScopedEnv::new();
    installed(&mut environment, "so.tilo.slotrows", &payload);
    installed(&mut environment, "so.tilo.window-menu", &probe_binary());
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
    until(&mut session, "draws the menu", |session| {
        session.surfaces().running("window-menu")
    });

    // The view the person bound shows every row the menu offers, each labelled as the menu labels
    // it, the rows of its submenus with them: the rows of another app's reading read as the records
    // the desk declares, and a submenu is a row of it rather than a second reading to walk.
    let offered = rows(&session);
    bind(&mut session, "so.tilo.slotrows", "menu");
    until(&mut session, "shows the rows the menu offers", |session| {
        draws_every_row(session, &offered)
    });
    let shares = row_label(&offered, "window-width");
    assert!(
        session
            .slot_control("window-menu", &row_label(&offered, "window-width-wide"))
            .is_some(),
        "the view reaches a row inside `{shares}`: {:?}",
        session.slot_labels("window-menu")
    );

    // The person presses the row that maximizes the window: the view's own `choose` , through the
    // session's door, runs the command the row means and the menu closes.
    press(&mut session, &row_label(&offered, "window-maximize"));
    until(
        &mut session,
        "chooses the row the person pressed",
        |session| {
            session
                .read(WINDOW_MENU)
                .is_some_and(|reading| reading.get("tab").is_some_and(Value::is_null))
        },
    );

    // The window is maximized now, which only the row's command does: the menu opened again offers
    // its opposite, and the view draws that.
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &json!({ "tab": "20" })
                .as_object()
                .expect("an object")
                .clone(),
        )
        .expect("the menu opens again");
    let reopened = rows(&session);
    assert!(
        reopened
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row["id"] == json!("window-restore")),
        "the row the view chose maximized the window: {reopened}"
    );
    let restore = row_label(&reopened, "window-restore");
    until(
        &mut session,
        "draws the rows the window offers now",
        |session| session.slot_control("window-menu", &restore).is_some(),
    );
}
