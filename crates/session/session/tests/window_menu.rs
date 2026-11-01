//! Super+F10 over the real session: the window in front's menu is the
//! session's `desktop:window-menu` reading, Escape closes it without
//! choosing, and choosing a row sends the command that row means — the same
//! menu a view draws, a script reads and an agent drives.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::mount::shell::WINDOW_MENU;
use tilo_session::{Options, Session};

/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_F10: u32 = 68;
const KEY_LEFT_META: u32 = 125;

/// The rows the desk offers a lone window, in order.
const ROWS: [&str; 14] = [
    "Full screen",
    "Fill the strip",
    "Move to strip",
    "Move to workspace",
    "Move to display",
    "Half the strip",
    "Narrow",
    "Normal",
    "Wide",
    "Every window the same",
    "Wider",
    "Narrower",
    "New tab of this app",
    "Close this window",
];

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

/// The desk the session starts on: one window, in front, on one strip.
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

/// A headless session showing `tree`, with a turn taken so its desk has laid
/// the windows out.
fn session_with(tree: DesktopTree) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    session
}

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(REFRESH).expect("step");
}

fn tap(session: &mut Session, keycode: u32) {
    key(session, keycode, true);
    key(session, keycode, false);
}

/// Super+F10, the window menu's chord.
fn press_tab_menu(session: &mut Session) {
    key(session, KEY_LEFT_META, true);
    tap(session, KEY_F10);
    key(session, KEY_LEFT_META, false);
}

/// The arguments of an invocation, as JSON spells them.
fn args(arguments: &Value) -> serde_json::Map<String, Value> {
    arguments.as_object().expect("an object").clone()
}

/// The window menu as the session reads it now.
fn menu(session: &Session) -> Value {
    session
        .read(WINDOW_MENU)
        .expect("the session publishes the window menu")
}

/// The labels of the menu's rows, in order; nothing while it is closed.
fn labels(reading: &Value) -> Vec<String> {
    reading["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["label"].as_str().expect("a label").to_owned())
        .collect()
}

/// The window's tab as the session publishes it, or `None` while the menu is
/// closed.
fn menu_tab(reading: &Value) -> Option<String> {
    reading["tab"].as_str().map(str::to_owned)
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
fn super_f10_opens_the_window_in_fronts_menu_at_its_window() {
    let mut session = session_with(desk_tree());
    assert!(menu_tab(&menu(&session)).is_none(), "nothing is up yet");

    press_tab_menu(&mut session);

    let reading = menu(&session);
    assert_eq!(
        menu_tab(&reading).as_deref(),
        Some("20"),
        "for the window in front"
    );
    assert_eq!(
        labels(&reading),
        ROWS,
        "with the desk's own rows, in its own order"
    );
    assert!(
        reading["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .all(|row| row["enabled"].is_boolean() && row["id"].is_string()),
        "every row says what choosing it does and whether it can be"
    );

    // Where the menu stands is the desk's to say: the corner of the window
    // it is for, drawn nothing else having an opinion, or the place the
    // person named.
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &args(&json!({ "tab": "20", "anchor": "40,50" })),
        )
        .expect("the menu opens where the person put it");
    let reading = menu(&session);
    assert_eq!(
        (
            reading["anchor"]["x"].as_f64().expect("x"),
            reading["anchor"]["y"].as_f64().expect("y")
        ),
        (40.0, 50.0),
        "where it was asked to stand"
    );

    // Choosing the window's own close does what the same command does from
    // anywhere: the window is gone and the menu with it.
    session
        .invoke(
            WINDOW_MENU,
            "choose",
            &args(&json!({ "row": "window-close-window" })),
        )
        .expect("the row is taken");
    // The desk carries out what it was asked in its next turn.
    session.step(REFRESH).expect("a turn of the session");
    assert!(!holds(&session, 20), "the window closed");
    assert!(
        menu_tab(&menu(&session)).is_none(),
        "and the menu closed with it"
    );
}

#[test]
fn escape_and_the_shortcut_again_close_the_menu_without_choosing() {
    let mut session = session_with(desk_tree());

    press_tab_menu(&mut session);
    assert_eq!(menu_tab(&menu(&session)).as_deref(), Some("20"));

    tap(&mut session, KEY_ESCAPE);
    let reading = menu(&session);
    assert!(
        menu_tab(&reading).is_none(),
        "Escape closes the menu: {reading}"
    );
    assert!(labels(&reading).is_empty(), "with no rows left to choose");
    assert!(holds(&session, 20), "and leaves the window alone");

    press_tab_menu(&mut session);
    assert_eq!(menu_tab(&menu(&session)).as_deref(), Some("20"));
    press_tab_menu(&mut session);
    assert!(
        menu_tab(&menu(&session)).is_none(),
        "the shortcut again closes what it opened"
    );
}

#[test]
fn the_menu_follows_the_window_in_front() {
    let mut session = session_with(desk_tree());
    let mut tree = session.handle().read(Clone::clone);
    if let Some(strip) = tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .find(|strip| strip.id == StripId(2))
    {
        strip.window_groups.push(WindowGroup {
            id: 4,
            active_tab: 21,
            tabs: vec![window(21, "Mail", "so.tilo.mail")],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    tree.active_group.insert(StripId(2), 4);
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");

    press_tab_menu(&mut session);

    assert_eq!(
        menu_tab(&menu(&session)).as_deref(),
        Some("21"),
        "the menu is the one in front, not the first"
    );
}

#[test]
fn a_menu_of_a_window_that_left_closes_and_offers_nothing() {
    let mut session = session_with(desk_tree());
    let mut tree = session.handle().read(Clone::clone);
    session
        .invoke(WINDOW_MENU, "open", &args(&json!({ "tab": "20" })))
        .expect("the menu opens for the window named");
    assert_eq!(menu_tab(&menu(&session)).as_deref(), Some("20"));

    // The window goes, by the desk's own command, while its menu is up.
    tree.workspaces[0].strips[0].window_groups.clear();
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");

    assert!(
        labels(&menu(&session)).is_empty(),
        "a window that is gone offers nothing"
    );
    let row = session.invoke(
        WINDOW_MENU,
        "choose",
        &args(&json!({ "row": "window-close-window" })),
    );
    assert!(row.is_err(), "and nothing is left to choose");
}

#[test]
fn a_row_the_window_cannot_choose_reads_with_its_reason() {
    let mut session = session_with(desk_tree());
    // A place with nowhere to go keeps its row, disabled with the reason,
    // rather than vanishing: the menu reads the same for every window.
    session
        .invoke(WINDOW_MENU, "open", &args(&json!({ "tab": "20" })))
        .expect("the menu opens");
    let reading = menu(&session);
    let rows = reading["rows"].as_array().expect("rows");
    assert!(
        rows.iter().any(|row| row["enabled"] == json!(true)),
        "the window offers what it can do: {reading}"
    );
    assert!(
        rows.iter()
            .filter(|row| row["enabled"] == json!(false))
            .all(|row| row["reason"].is_string()),
        "and says why for each it cannot: {reading}"
    );
}
