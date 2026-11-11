//! The surfaces over the desk as the session publishes them: what stands where, which of them holds
//! the keyboard, what Escape closes and what a press outside one closes.
//! The stack is the one answer every overlay used to give for itself; these tests hold the session
//! to it — the window menu and quick settings, the two surfaces the session puts up itself, close
//! by the dismissal table, the keyboard goes back to the window as the last of them closes, and a
//! popover stands where its anchor and its size say, under the bar's trailing end or beside the
//! caret that asked for it.

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

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop::topbar::TOPBAR_HEIGHT_PX;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_session::shell::state::{FOCUS, WINDOW_MENU};
use tilo_session::surfaces::Surface;
use tilo_session::{Options, Session, quick_settings};
use tilo_session_core::shell::OVERLAYS;
use tilo_test_env::ScopedEnv;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;

/// The display the session runs on.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The gap a popover leaves between itself and the edges of its display, as the stack holds it.
const MARGIN: f64 = 8.0;

/// The width of a popover under the bar.
const POPOVER_WIDTH: f64 = 380.0;

/// The height of a popover under the bar: its footprint, which no page it shows moves.
const POPOVER_HEIGHT: f64 = 640.0;

/// A coordinate the stack published stands where this test expects it, to the pixel the surface is
/// drawn at.
#[track_caller]
fn near(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 0.5,
        "{what}: {actual} is not {expected}"
    );
}

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

/// A headless session showing `tree` , with a turn taken so its desk has laid the windows out.
fn session_with(tree: DesktopTree, panel: Option<Surface>) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            surfaces: panel.into_iter().collect(),
            collab: false,
            ..Options::default()
        },
    );
    session.handle().replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    session
}

/// The session with no quick settings app installed.
fn session() -> Session {
    session_with(desk_tree(), None)
}

/// The panel's app for this package's tests: the surface probe the session package ships, standing
/// in for a view that draws the panel and staying up while the session keeps it.
fn panel_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
}

/// The environment its stand-in app reads, held for the test's life: it draws nothing, and stays up
/// until the session stops it.
fn panel_environment() -> ScopedEnv {
    let mut environment = ScopedEnv::new();
    environment.set("SURFACE_PROBE_STAY", "1");
    environment
}

/// The session with the quick settings panel installed and its app running while the stack has the
/// popover up.
fn session_with_quick_settings() -> (Session, ScopedEnv) {
    let environment = panel_environment();
    let session = session_with(
        desk_tree(),
        Some(quick_settings::surface_of(panel_binary())),
    );
    (session, environment)
}

/// The arguments of an invocation, as JSON spells them.
fn args(arguments: &Value) -> serde_json::Map<String, Value> {
    arguments.as_object().expect("an object").clone()
}

/// Open the surface `slot` , hanging where `anchor` says, through the same verb a view or an agent
/// invokes.
fn open(session: &mut Session, slot: &str, anchor: Option<&str>) {
    let arguments = match anchor {
        Some(anchor) => json!({ "slot": slot, "anchor": anchor }),
        None => json!({ "slot": slot }),
    };
    session
        .invoke(OVERLAYS, "open", &args(&arguments))
        .expect("the surface opens");
    session.step(REFRESH).expect("a turn of the session");
}

/// The surfaces over the desk now, topmost first.
fn stack(session: &Session) -> Vec<Value> {
    session
        .read(OVERLAYS)
        .expect("the session publishes its surfaces")["stack"]
        .as_array()
        .expect("a stack")
        .clone()
}

/// The slots of the surfaces up now, topmost first.
fn slots(session: &Session) -> Vec<String> {
    stack(session)
        .iter()
        .map(|surface| surface["slot"].as_str().expect("a slot").to_owned())
        .collect()
}

/// The one open surface of `slot` .
fn surface(session: &Session, slot: &str) -> Value {
    stack(session)
        .into_iter()
        .find(|surface| surface["slot"] == json!(slot))
        .unwrap_or_else(|| panic!("{slot} is up: {:?}", stack(session)))
}

/// Where the stack places a surface, as `(x, y, w, h)` .
fn place(session: &Session, slot: &str) -> (f64, f64, f64, f64) {
    let place = &surface(session, slot)["place"];
    (
        place["x"].as_f64().expect("x"),
        place["y"].as_f64().expect("y"),
        place["w"].as_f64().expect("w"),
        place["h"].as_f64().expect("h"),
    )
}

/// What holds the keyboard, as the session publishes it.
fn focus(session: &Session) -> String {
    session
        .read(FOCUS)
        .expect("the session publishes its focus")["target"]
        .as_str()
        .expect("a target")
        .to_owned()
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

/// One click delivered in one go: the button down and up before the session carries out what the
/// press asked, as a quick tap of a trackpad does.
fn quick_click(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
    }
}

/// A press at `(x, y)` on the desk, wherever that lands.
fn press(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(REFRESH).expect("step");
    }
}

#[test]
fn the_session_publishes_what_it_puts_up_and_who_holds_the_keyboard() {
    let mut session = session();
    assert!(
        slots(&session).is_empty(),
        "nothing stands over the desk yet"
    );
    assert_eq!(
        focus(&session),
        "window",
        "the window in front has the keys"
    );

    // The window menu, at the place the person put it.
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &args(&json!({ "tab": "20", "anchor": "40,50" })),
        )
        .expect("the menu opens");
    session.step(REFRESH).expect("a turn of the session");
    let menu = surface(&session, "window-menu");
    assert_eq!(menu["slot"], json!("window-menu"));
    assert_eq!(menu["role"], json!("popup"));
    assert_eq!(menu["anchor"], json!("at:40,50"), "where it was put");
    assert_eq!(
        menu["place"],
        Value::Null,
        "the menu's own view places it; the session judges no rect"
    );
    assert_eq!(focus(&session), "overlay:window-menu");

    // And quick settings under the bar's trailing end: one anchored surface at a time, so the menu
    // goes down for the panel.
    session
        .invoke(
            OVERLAYS,
            "open",
            &args(&json!({ "slot": "quick-settings" })),
        )
        .expect("the panel opens");
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(slots(&session), ["quick-settings"]);
    assert_eq!(
        surface(&session, "quick-settings")["anchor"],
        json!("top-trailing:8")
    );
    assert_eq!(
        focus(&session),
        "overlay:quick-settings",
        "the surface the person just opened holds the keys"
    );

    session
        .invoke(
            OVERLAYS,
            "close",
            &args(&json!({ "slot": "quick-settings" })),
        )
        .expect("the panel closes");
    session.step(REFRESH).expect("a turn of the session");
    assert!(slots(&session).is_empty());
    assert_eq!(focus(&session), "window", "and the window again");
}

#[test]
fn escape_closes_the_topmost_surface_that_answers_it() {
    let (mut session, _panel) = session_with_quick_settings();
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &args(&json!({ "tab": "20", "anchor": "40,50" })),
        )
        .expect("the menu opens");
    open(&mut session, "quick-settings", None);
    assert_eq!(
        slots(&session),
        ["quick-settings"],
        "the panel takes the place of the menu"
    );

    // Escape in the panel is the panel's: it goes back a page inside it before it closes (the shell
    // proves that with the real app). Here the stack says the menu below went down for it.
    session
        .invoke(
            OVERLAYS,
            "close",
            &args(&json!({ "slot": "quick-settings" })),
        )
        .expect("the panel closes");
    session.step(REFRESH).expect("a turn of the session");
    assert!(slots(&session).is_empty());
    assert_eq!(focus(&session), "window", "and the window again");

    // A menu stands alone, and Escape closes it.
    session
        .invoke(
            WINDOW_MENU,
            "open",
            &args(&json!({ "tab": "20", "anchor": "40,50" })),
        )
        .expect("the menu opens again");
    assert_eq!(slots(&session), ["window-menu"]);
    tap(&mut session, KEY_ESCAPE);
    assert!(slots(&session).is_empty(), "the menu closed");
    assert_eq!(focus(&session), "window");
}

#[test]
fn a_surface_the_desktops_own_chrome_shows_is_not_the_sessions_to_open() {
    // The menus, the picker, the overview and the strip ribbon open and close with the widgets that
    // draw them, which say so to the same stack: the session's verb refuses them rather than
    // putting up a second one.
    let mut session = session();
    for slot in ["emoji", "workspace-menu", "tilo-menu", "overview", "strips"] {
        let opened = session.invoke(OVERLAYS, "open", &args(&json!({ "slot": slot })));
        assert!(opened.is_err(), "{slot} is the chrome's");
        let closed = session.invoke(OVERLAYS, "close", &args(&json!({ "slot": slot })));
        assert!(closed.is_err(), "{slot} is the chrome's");
    }
    assert!(
        session
            .invoke(OVERLAYS, "open", &args(&json!({ "slot": "nowhere" })))
            .is_err(),
        "and no verb opens what no surface names"
    );
    assert!(slots(&session).is_empty());
}

#[test]
fn a_press_on_the_desk_closes_the_workspace_menu_the_chrome_hangs_off_its_chip() {
    // The chrome draws the menu and says where it stands; a press on the desk never reaches the
    // chrome drawn over it, so the session's reader of presses outside is what closes the menu.
    let mut session = session();
    let chip = (86.0, f64::from(TOPBAR_HEIGHT_PX) / 2.0);
    press(&mut session, chip.0, chip.1);
    assert_eq!(slots(&session), ["workspace-menu"], "the chip opened it");
    // Its release comes up before the chrome takes its next pass, as a quick click's does.
    quick_click(&mut session, 100.0, 400.0);
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        slots(&session).is_empty(),
        "the press on the desk closed it"
    );
    // And the menu it draws followed the stack: the chip opens it again rather than closing the one
    // still standing under the person's finger.
    press(&mut session, chip.0, chip.1);
    assert_eq!(
        slots(&session),
        ["workspace-menu"],
        "the chip opens it again"
    );
}

#[test]
fn a_popover_stands_under_the_bars_right_end_and_beside_the_caret_that_asked() {
    let (mut session, _panel) = session_with_quick_settings();
    let bar = f64::from(TOPBAR_HEIGHT_PX).ceil();

    open(&mut session, "quick-settings", None);
    let (x, y, w, h) = place(&session, "quick-settings");
    near(
        x,
        f64::from(WIDTH) - MARGIN - POPOVER_WIDTH,
        "against the right end",
    );
    near(y, bar, "under the bar");
    near(w, POPOVER_WIDTH, "the stack's width");
    near(
        h,
        POPOVER_HEIGHT,
        "its footprint, not the rest of the display",
    );
    assert!(
        y + h < f64::from(HEIGHT) - MARGIN,
        "with the desk it does not cover left below it"
    );
    assert_eq!(
        surface(&session, "quick-settings")["size"],
        json!({ "width": POPOVER_WIDTH, "height": POPOVER_HEIGHT }),
        "the footprint the surface asks for"
    );

    // Beside a caret, clear of the line it sits on: the popover stands below that line and takes
    // the room under it, down to the margin, so what it draws never covers what the person types.
    open(
        &mut session,
        "quick-settings",
        Some("beside:600,300,602,320,4"),
    );
    let (x, y, w, h) = place(&session, "quick-settings");
    near(x, 606.0, "the gap clear of the caret");
    near(y, 324.0, "clear below the caret's line");
    near(
        y + h,
        f64::from(HEIGHT) - MARGIN,
        "the room under the line, down to the margin",
    );
    near(w, POPOVER_WIDTH, "the stack's width");

    // With no room on the right of the caret, it opens on the left of it.
    open(
        &mut session,
        "quick-settings",
        Some("beside:1200,300,1202,320,4"),
    );
    let (x, ..) = place(&session, "quick-settings");
    assert!(x + POPOVER_WIDTH <= 1200.0, "clear on the left: {x}");

    // A place it cannot parse is refused rather than guessed.
    assert!(
        session
            .invoke(
                OVERLAYS,
                "open",
                &args(&json!({ "slot": "quick-settings", "anchor": "nowhere" })),
            )
            .is_err(),
        "no such place"
    );
}

/// The panel's app starts with the session, hidden, so the first opening only shows it; the stack
/// places it, and opening it somewhere else moves the surface it stands on.
#[test]
fn the_panel_runs_before_its_first_opening_and_moves_where_the_stack_places_it() {
    let (mut session, _panel) = session_with_quick_settings();
    assert!(
        session.surfaces().running("quick-settings"),
        "the app runs before the panel is first opened"
    );
    assert_eq!(
        session
            .compositor()
            .layer_shown(quick_settings::SURFACE_NAMESPACE),
        Some(false),
        "hidden, drawing nothing"
    );
    open(&mut session, "quick-settings", None);
    assert!(session.surfaces().running("quick-settings"));
    open(&mut session, "quick-settings", Some("top-leading:8"));
    assert!(
        session.surfaces().running("quick-settings"),
        "it stands where the stack places it now"
    );
}

#[test]
fn the_panel_stands_hidden_between_openings_and_the_next_opening_starts_nothing() {
    let (mut session, _panel) = session_with_quick_settings();
    open(&mut session, "quick-settings", None);
    assert!(
        session.surfaces().running("quick-settings"),
        "the app draws the panel"
    );
    assert_eq!(
        session
            .compositor()
            .layer_shown(quick_settings::SURFACE_NAMESPACE),
        Some(true),
        "and the compositor shows it"
    );

    // Closed: the app keeps running, hidden, so the next opening draws the panel instead of
    // starting it.
    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "quick-settings",
            &args(&json!({})),
        )
        .expect("the bar asks for the panel");
    session.step(REFRESH).expect("a turn of the session");
    assert!(!session.quick_settings_open());
    assert!(
        session.surfaces().running("quick-settings"),
        "the panel's app is resident"
    );
    assert_eq!(
        session
            .compositor()
            .layer_shown(quick_settings::SURFACE_NAMESPACE),
        Some(false),
        "and hidden, drawing nothing and keeping no room"
    );

    open(&mut session, "quick-settings", None);
    assert!(session.quick_settings_open());
    assert!(
        session.surfaces().running("quick-settings"),
        "the opening started nothing"
    );
    assert_eq!(
        session
            .compositor()
            .layer_shown(quick_settings::SURFACE_NAMESPACE),
        Some(true),
        "the surface it stands on is shown again"
    );
}

#[test]
fn a_press_outside_the_popover_closes_it_and_one_inside_it_does_not() {
    let (mut session, _panel) = session_with_quick_settings();
    open(&mut session, "quick-settings", None);
    assert!(session.quick_settings_open());

    // Inside the panel: the person works in it, and it stays.
    let (x, y, w, h) = place(&session, "quick-settings");
    press(&mut session, x + w / 2.0, y + h / 2.0);
    assert!(
        session.quick_settings_open(),
        "a press in the panel is the panel's"
    );

    // Anywhere else on the desk: it closes, and what the press landed on still gets it.
    press(&mut session, 100.0, 400.0);
    assert!(
        !session.quick_settings_open(),
        "the press outside closed it"
    );
    assert!(slots(&session).is_empty());
    assert_eq!(focus(&session), "window");
}

#[test]
fn a_dismissing_press_does_not_reopen_what_it_closed() {
    let (mut session, _panel) = session_with_quick_settings();
    open(&mut session, "quick-settings", None);
    // The person's finger goes down on the bar, outside the panel: the panel closes, and the bar's
    // own toggle — raised by that same press — must not put it back up under the finger.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 100.0, y: 400.0 })
        .expect("the pointer");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        })
        .expect("the button");
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        !session.quick_settings_open(),
        "the press outside closed it"
    );
    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "quick-settings",
            &args(&json!({})),
        )
        .expect("the bar asks for the panel");
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        !session.quick_settings_open(),
        "the press that closed it did not open it again"
    );

    // The finger comes up: the press is over, and the next ask opens it.
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        })
        .expect("the button");
    session.step(REFRESH).expect("a turn of the session");
    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "quick-settings",
            &args(&json!({})),
        )
        .expect("the bar asks for the panel");
    session.step(REFRESH).expect("a turn of the session");
    assert!(session.quick_settings_open(), "a press of its own opens it");
}

#[test]
fn a_quick_click_on_the_bar_closes_the_panel_and_leaves_it_closed() {
    let (mut session, _panel) = session_with_quick_settings();
    open(&mut session, "quick-settings", None);

    // The person taps the status cluster's readings, at the bar's trailing end beyond the bell the
    // cluster opens with: the click comes up before the session has carried out the toggle its
    // press raised, and the press that closed the panel still does not open it again.
    let button = (f64::from(WIDTH) - 6.0, f64::from(TOPBAR_HEIGHT_PX) / 2.0);
    quick_click(&mut session, button.0, button.1);
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        !session.quick_settings_open(),
        "the click that closed the panel did not open it again"
    );

    // The next click is its own: it opens the panel again.
    quick_click(&mut session, button.0, button.1);
    session.step(REFRESH).expect("a turn of the session");
    assert!(session.quick_settings_open(), "a click of its own opens it");
}
