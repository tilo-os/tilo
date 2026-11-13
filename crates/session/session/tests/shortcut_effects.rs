//! The shortcuts of the one table on a real session: the chords the session answers open the
//! surface each names, and the chords the desk answers move its windows — pressed as a person
//! presses them, through the keyboard the compositor gives the session.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::path::PathBuf;
use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
/// The platform search the desk opens on its chord.
const HOST_SEARCH: &str = "tilo.host:search";
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, SEARCH_RESOURCE};
use tilo_desktop_model::{
    AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_session::shell::state::WINDOW_MENU;
use tilo_session::{Options, Session, SessionConfig, quick_settings};
use tilo_sheets::Seat as _;
use tilo_test_env::ScopedEnv;
use ui::shortcut_table::{KeyChord, ShortcutAction, ShortcutConfig, ShortcutHome, fixed_shortcut};

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_8: u32 = 9;
const KEY_MINUS: u32 = 12;
const KEY_EQUAL: u32 = 13;
const KEY_E: u32 = 18;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_Q: u32 = 16;
const KEY_W: u32 = 17;
const KEY_R: u32 = 19;
const KEY_A: u32 = 30;
const KEY_D: u32 = 32;
const KEY_F: u32 = 33;
const KEY_L: u32 = 38;
const KEY_C: u32 = 46;
const KEY_V: u32 = 47;
const KEY_SLASH: u32 = 53;
const KEY_SPACE: u32 = 57;
const KEY_F4: u32 = 62;
const KEY_F10: u32 = 68;
const KEY_DELETE: u32 = 111;
const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_LEFT_ALT: u32 = 56;
const KEY_LEFT_META: u32 = 125;

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

/// The desk the session starts on: one strip of two windows, the first with two tabs.
fn desk_tree() -> DesktopTree {
    let first = WindowGroup {
        id: 3,
        active_tab: 20,
        tabs: vec![
            window(20, "Notes", "so.tilo.notes"),
            window(21, "Mail", "so.tilo.mail"),
        ],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let second = WindowGroup {
        id: 4,
        active_tab: 30,
        tabs: vec![window(30, "Chat", "so.tilo.chat")],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![first, second],
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

/// The panel's app, as the session starts it: a stand-in that stays up while the stack has the
/// panel open.
fn panel_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
}

/// What the stand-in panel reads so it stays up until the session stops it,
/// held for the test's life.
fn panel_environment() -> ScopedEnv {
    let mut environment = ScopedEnv::new();
    environment.set("SURFACE_PROBE_STAY", "1");
    environment
}

fn session_with(tree: DesktopTree) -> Session {
    session_with_config(tree, SessionConfig::default())
}

fn session_with_config(tree: DesktopTree, config: SessionConfig) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
        Options {
            surfaces: vec![quick_settings::surface_of(panel_binary())],
            collab: false,
            config,
            ..Options::default()
        },
    );
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

/// The keycode a default chord's key sends.
fn keycode(name: &str) -> u32 {
    match name {
        "space" => KEY_SPACE,
        "enter" => KEY_ENTER,
        "e" => KEY_E,
        "tab" => KEY_TAB,
        "l" => KEY_L,
        "q" => KEY_Q,
        "w" => KEY_W,
        "r" => KEY_R,
        "a" => KEY_A,
        "c" => KEY_C,
        "v" => KEY_V,
        "f" => KEY_F,
        "d" => KEY_D,
        "/" => KEY_SLASH,
        "-" => KEY_MINUS,
        "=" => KEY_EQUAL,
        "8" => KEY_8,
        "f4" => KEY_F4,
        "f10" => KEY_F10,
        "delete" => KEY_DELETE,
        "escape" => KEY_ESCAPE,
        other => panic!("no keycode for {other}"),
    }
}

/// The modifier keycodes a chord names.
fn modifier_keys(chord: &KeyChord) -> Vec<u32> {
    chord
        .modifiers
        .iter()
        .map(|name| match name.as_str() {
            "Super" => KEY_LEFT_META,
            "Ctrl" => KEY_LEFT_CTRL,
            "Alt" => KEY_LEFT_ALT,
            "Shift" => KEY_LEFT_SHIFT,
            other => panic!("no keycode for {other}"),
        })
        .collect()
}

/// Press the chord the person's table binds `action` to, as a person does: the modifiers held, the
/// key tapped, the modifiers let go.
fn press(session: &mut Session, action: ShortcutAction) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    press_chord(session, &chord);
}

/// Press `chord` as a person does, through the keyboard the compositor gives the session.
fn press_chord(session: &mut Session, chord: &KeyChord) {
    let held = modifier_keys(chord);
    for keycode in &held {
        key(session, *keycode, true);
    }
    let pressed = keycode(&chord.key);
    key(session, pressed, true);
    key(session, pressed, false);
    for keycode in held.iter().rev() {
        key(session, *keycode, false);
    }
}

/// Step the session until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..120 {
        if done(session) {
            return;
        }
        session.step(REFRESH).expect("a turn of the session");
    }
    panic!("{what} never happened");
}

/// Whether the desk publishes the resource `id` now: the open search, say.
fn published(session: &Session, id: &str) -> bool {
    fn find(resources: &[tilo_actions::ResourceSpec], id: &str) -> bool {
        resources
            .iter()
            .any(|resource| resource.id.as_str() == id || find(&resource.children, id))
    }
    find(&session.resources(), id)
}

/// What the session shows of the search it opened.
fn searching(session: &Session) -> bool {
    published(session, HOST_SEARCH)
}

/// Whether the desk's own search is open: it publishes it while it is, and the host search door
/// (`tilo.host:search`) is published whenever the search app is there at all.
fn search_open(session: &Session) -> bool {
    published(session, SEARCH_RESOURCE)
}

/// The tab the window menu is up for, while it is.
fn menu_tab(session: &Session) -> Option<String> {
    session
        .read(WINDOW_MENU)?
        .get("tab")?
        .as_str()
        .map(str::to_owned)
}

/// Write the screen as `<dir>/<name>.png` when the person running the test asks for captures: what
/// the chord looks like where they read it.
fn capture(session: &mut Session, name: &str) {
    let Some(dir) = std::env::var_os("SHORTCUT_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("the shots directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
}

/// Type `text` as the keyboard attached to the seat types it.
fn type_at_the_seat(session: &mut Session, text: &str) {
    for ch in text.chars() {
        assert!(
            session.compositor().type_char(ch).expect("input"),
            "the seat's layout types {ch:?}"
        );
        for _ in 0..2 {
            session.step(Duration::from_millis(12)).expect("step");
        }
    }
    for _ in 0..20 {
        session.step(REFRESH).expect("a turn of the session");
    }
}

fn guide_up(session: &Session) -> bool {
    session
        .shortcut_guide()
        .is_some_and(|seat| seat.presentation().is_open())
}

/// Every app the desk shows a window of, or holds a window for while its app arrives.
fn apps(session: &Session) -> Vec<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match &tab.tile {
                TileContent::LocalApp { app_id, .. } => Some(app_id.clone()),
                TileContent::AppPlaceholder {
                    kind: AppKind::Other(app),
                    ..
                } => Some(app.clone()),
                _ => None,
            })
            .collect()
    })
}

/// Every shortcut of the session's side opens or starts what it names: the search, the window in
/// front's menu, the guide, quick settings and its pages, a conversation, and the magnifier's zoom.
#[test]
fn the_session_answers_every_shortcut_of_its_side() {
    let _panel = panel_environment();
    for action in ShortcutAction::ALL
        .into_iter()
        .filter(|action| action.home() == ShortcutHome::Session)
    {
        // The ones the session carries out through a service or a surface
        // this run has none of (the power menu needs the power service, the
        // lock screen a lock surface, dictation a voice, the next input
        // source a second keyboard layout) are those services' to prove;
        // each was handed on whole, which the desk's own test does.
        if matches!(
            action,
            ShortcutAction::PowerMenu
                | ShortcutAction::Lock
                | ShortcutAction::NextInputSource
                | ShortcutAction::DictationToggle
                | ShortcutAction::DictationFinish
                | ShortcutAction::DictationCancel
                // A report goes to the one place this computer is told,
                // and this run is told nowhere (`session/send_key.rs` asks
                // the window in front where it is).
                | ShortcutAction::ReportProblem
        ) {
            continue;
        }
        let mut session = session_with(desk_tree());
        // Zooming out from the magnifier's off has nothing to zoom out of: the chord that turned it
        // on gives it something.
        if action == ShortcutAction::MagnifierZoomOut {
            press(&mut session, ShortcutAction::ToggleMagnifier);
        }
        let zoom = session.compositor().zoom();
        press(&mut session, action);
        match action {
            ShortcutAction::Search => {
                until(&mut session, "the search", |session| searching(session));
            }
            ShortcutAction::TabMenu => until(&mut session, "the window menu", |session| {
                menu_tab(session).as_deref() == Some("20")
            }),
            ShortcutAction::ShortcutHelp => {
                until(&mut session, "the guide", |session| guide_up(session));
            }
            ShortcutAction::QuickSettings | ShortcutAction::ClipboardHistory => {
                until(&mut session, "quick settings", |session| {
                    session.quick_settings_open()
                });
            }
            // The bell opens the one list of notifications: the centre over the desk, not a page of
            // quick settings.
            ShortcutAction::Notifications => {
                until(&mut session, "the notification centre", |session| {
                    session
                        .accessible_labels()
                        .iter()
                        .any(|label| label == "Retained notifications")
                });
            }
            ShortcutAction::AskAgent | ShortcutAction::SaySomething => {
                until(&mut session, "a conversation", |session| {
                    apps(session).iter().any(|app| app == "so.tilo.chat")
                });
            }
            ShortcutAction::ToggleMagnifier => {
                until(&mut session, "the magnifier", |session| {
                    session.compositor().zoom() != zoom
                });
            }
            ShortcutAction::MagnifierZoomIn => {
                until(&mut session, "the magnifier's zoom", |session| {
                    session.compositor().zoom() > zoom
                });
            }
            ShortcutAction::MagnifierZoomOut => {
                until(&mut session, "the magnifier's zoom", |session| {
                    session.compositor().zoom() < zoom
                });
            }
            other => panic!("{other:?} is the session's and has no proof here"),
        }
    }
}

/// The clipboard panel Super+V asks for is up in the pass the key is pressed in, beside a caret
/// nothing waits for: no frame goes by with the key answered and the panel still down.
#[test]
fn super_v_puts_the_clipboard_panel_up_in_the_pass_it_is_pressed_in() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    key(&mut session, KEY_LEFT_META, true);
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: KEY_V,
            pressed: true,
        })
        .expect("the key");
    session.step(REFRESH).expect("a pass");
    assert!(
        session.quick_settings_open(),
        "the pass that ran Super+V put the panel up"
    );
}

/// Tapping Super alone opens search: a press and a release with nothing between, well inside the
/// hold's threshold. A chord with it does not: the second key makes Super that chord's modifier,
/// so Super+W closes the window in front and opens no search.
#[test]
fn a_tap_of_super_alone_opens_search_and_a_chord_does_not() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    key(&mut session, KEY_LEFT_META, true);
    key(&mut session, KEY_LEFT_META, false);
    until(&mut session, "the search", |session| search_open(session));

    // Super+W: the chord of the second key, not a tap of Super.
    let mut session = session_with(desk_tree());
    key(&mut session, KEY_LEFT_META, true);
    key(&mut session, KEY_W, true);
    key(&mut session, KEY_W, false);
    key(&mut session, KEY_LEFT_META, false);
    until(&mut session, "the window closed", |session| {
        session
            .handle()
            .read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs.len() == 1)
    });
    for _ in 0..10 {
        session.step(REFRESH).expect("a turn of the session");
    }
    assert!(
        !search_open(&session),
        "Super+W is a chord of Super's, not a tap of it"
    );
}

/// Holding Super alone shows the shortcut guide's keys, and letting it go takes them away again.
/// A key pressed while the hold shows them ends the hold's hints and still runs.
#[test]
fn holding_super_shows_the_keys_and_letting_go_takes_them_away() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    key(&mut session, KEY_LEFT_META, true);
    until(&mut session, "the keys", |session| guide_up(session));
    key(&mut session, KEY_LEFT_META, false);
    until(&mut session, "the keys gone", |session| !guide_up(session));

    // Held again, the hints go with the chord the person presses while holding: the window in
    // front closes, and the keys go rather than holding the chord back.
    key(&mut session, KEY_LEFT_META, true);
    until(&mut session, "the keys", |session| guide_up(session));
    key(&mut session, KEY_W, true);
    key(&mut session, KEY_W, false);
    until(&mut session, "the keys gone with the chord", |session| {
        !guide_up(session)
    });
    until(&mut session, "the window closed", |session| {
        session
            .handle()
            .read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs.len() == 1)
    });
    key(&mut session, KEY_LEFT_META, false);
}

/// A pointer action between Super's press and its release is no tap: the person was using the
/// pointer, not reaching for the search.
#[test]
fn a_pointer_action_between_supers_press_and_release_opens_no_search() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    key(&mut session, KEY_LEFT_META, true);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 640.0, y: 500.0 })
        .expect("the pointer");
    session
        .compositor()
        .input(InputEvent::PointerAxis {
            horizontal: 0.0,
            vertical: -15.0,
        })
        .expect("the wheel");
    key(&mut session, KEY_LEFT_META, false);
    for _ in 0..10 {
        session.step(REFRESH).expect("a turn of the session");
    }
    assert!(!search_open(&session), "Super and a wheel are no tap");
}

/// The chord a menu row shows, by the row's id.
fn row_chord(session: &Session, row: &str) -> Option<String> {
    let reading = session.read(WINDOW_MENU)?;
    reading
        .get("rows")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(serde_json::Value::as_str) == Some(row))?
        .get("shortcut")?
        .as_str()
        .map(str::to_owned)
}

/// The chord the catalogue fixes for `id` , in the system's own spelling: its Super form where it
/// binds one.
#[track_caller]
fn fixed_chord(id: &str) -> String {
    let fixed = fixed_shortcut(id).expect("the catalogue binds it");
    let chords = fixed.chords();
    chords
        .iter()
        .find(|chord| {
            chord
                .modifiers
                .iter()
                .any(|modifier| modifier.eq_ignore_ascii_case("super"))
        })
        .or_else(|| chords.first())
        .expect("a fixed shortcut names a chord")
        .display()
}

/// The resource `id` the session publishes, children searched too.
fn resource(session: &Session, id: &str) -> Option<tilo_actions::ResourceSpec> {
    fn find(
        resources: &[tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(&session.resources(), id)
}

/// The chord the desk's chrome publishes beside the action `verb` : the text the search paints at
/// the row's end.
fn search_chord(session: &Session, verb: &str) -> Option<String> {
    resource(session, CHROME_RESOURCE)?
        .actions
        .iter()
        .find(|action| action.id == verb)?
        .state
        .shortcut
        .clone()
}

/// The labels of the rows the open search shows.
fn search_rows(session: &Session) -> Vec<String> {
    resource(session, SEARCH_RESOURCE)
        .map(|search| search.children.into_iter().map(|row| row.label).collect())
        .unwrap_or_default()
}

/// Step the session until the search shows a row called `title` .
#[track_caller]
fn until_a_row(session: &mut Session, title: &str) {
    for _ in 0..120 {
        if search_rows(session)
            .iter()
            .any(|row| row.starts_with(title))
        {
            return;
        }
        session.step(REFRESH).expect("a turn of the session");
    }
    panic!("no row {title:?} among {:?}", search_rows(session));
}

/// The window menu says, at every row a shortcut does the same thing as,
/// the chord that does it — the one table's, so a chord the person rebound
/// reads as they bound it.
#[test]
fn the_window_menu_shows_the_chord_the_table_binds() {
    let _panel = panel_environment();
    let mine = KeyChord::parse("Ctrl+Alt+F").expect("a chord");
    let mut config = SessionConfig::default();
    config
        .shortcuts
        .set(ShortcutAction::ToggleFullscreen, mine.clone())
        .expect("no other shortcut holds it");
    let mut session = session_with_config(desk_tree(), config);

    press(&mut session, ShortcutAction::TabMenu);
    until(&mut session, "the window menu", |session| {
        menu_tab(session).as_deref() == Some("20")
    });

    // The reading spells the chord the way the table stores it (a key legend lowercases a bare
    // letter); the menu paints it in the keyboard's own legends.
    assert_eq!(
        row_chord(&session, "window-fullscreen"),
        Some(mine.display()),
        "the row says the chord the person bound"
    );
    let maximized = ShortcutConfig::default()
        .bindings
        .get(&ShortcutAction::MaximizeWindow)
        .expect("bound")
        .display();
    assert_eq!(
        row_chord(&session, "window-maximize").as_deref(),
        Some(maximized.as_str()),
        "a row without one of its own takes the table's"
    );
    let close = fixed_chord("fixed-close-tab");
    assert_eq!(
        row_chord(&session, "window-close").as_deref(),
        Some(close.as_str()),
        "closing a tab is a chord Settings cannot rebind, so the row shows it"
    );
    assert_eq!(
        row_chord(&session, "window-close-window"),
        None,
        "the window's own close, which shuts every tab, is no chord's command"
    );
    assert_eq!(
        row_chord(&session, "window-new-tab").as_deref(),
        Some(fixed_chord("fixed-duplicate-tab").as_str()),
        "opening the app's next window is the catalogue's chord too"
    );
    assert_eq!(
        row_chord(&session, "window-rename"),
        None,
        "a row no shortcut does says nothing"
    );
    capture(&mut session, "window-menu");

    // A window of one tab is its tab: closing either is the same command, so its Close row reads
    // the chord that closes a tab.
    let mut single = desk_tree();
    single.workspaces[0].strips[0].window_groups.truncate(1);
    single.workspaces[0].strips[0].window_groups[0]
        .tabs
        .truncate(1);
    let mut lone = session_with_config(single, SessionConfig::default());
    press(&mut lone, ShortcutAction::TabMenu);
    until(&mut lone, "the window menu", |session| {
        menu_tab(session).as_deref() == Some("20")
    });
    assert_eq!(
        row_chord(&lone, "window-close-window").as_deref(),
        Some(fixed_chord("fixed-close-tab").as_str()),
        "a window of one tab closes on the chord that closes its tab"
    );
    assert!(
        row_chord(&lone, "window-close").is_none(),
        "no tab to close"
    );
    capture(&mut lone, "window-menu-close");
}

/// A window's menu belongs to its window: one still up when the window in front closes goes with
/// it, rather than standing over the desk with nothing to say and holding what the person presses,
/// and the next window's menu opens as it always does.
#[test]
fn the_window_menu_closes_with_the_window_it_belongs_to() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    press(&mut session, ShortcutAction::TabMenu);
    until(&mut session, "the window menu", |session| {
        menu_tab(session).as_deref() == Some("20")
    });

    // The window in front goes by the chord that closes it, not by the menu's own row: the menu is
    // left behind holding a window that is gone.
    let close = fixed_shortcut("fixed-close-tab").expect("closing the window is fixed");
    press_chord(&mut session, &close.chords()[0]);
    until(&mut session, "the window in front to go", |session| {
        session
            .handle()
            .snapshot("session", vec![])
            .active
            .is_some_and(|route| route.tab != "20")
    });
    until(
        &mut session,
        "the menu to close with its window",
        |session| menu_tab(session).is_none(),
    );
    assert!(
        row_chord(&session, "window-close").is_none(),
        "no rows stand for the window that went"
    );

    // And the menu is a menu again for the window in front now: the record of the view that drew
    // the one that went does not stand in its way.
    press(&mut session, ShortcutAction::TabMenu);
    until(&mut session, "the next window's menu", |session| {
        menu_tab(session).as_deref() == Some("21")
    });
    assert!(
        row_chord(&session, "window-maximize").is_some(),
        "the menu offers the window in front its rows"
    );
}

/// The search a person opens lists the actions a shortcut runs with the chord the one table binds
/// it to, which the row paints in the keyboard's legends.
/// The desk publishes that chord beside each action it offers a search, and the context menu and
/// the search paint the text it publishes: rebinding reaches it as it reaches the menus, which the
/// desk's own test follows.
#[test]
fn the_search_shows_the_chord_beside_the_action() {
    let _panel = panel_environment();
    let mut session = session_with(desk_tree());
    let chord = ShortcutConfig::default()
        .bindings
        .get(&ShortcutAction::ReopenClosedTab)
        .expect("bound")
        .display();
    assert_eq!(
        search_chord(&session, "reopen-closed").as_deref(),
        Some(chord.as_str()),
        "the action the search offers carries the chord the table binds"
    );
    press(&mut session, ShortcutAction::Search);
    until(&mut session, "the search", |session| searching(session));
    type_at_the_seat(&mut session, "reopen");
    until_a_row(&mut session, "Reopen the closed window");
    capture(&mut session, "search");
}

/// The chords of the desk's side reach the desk of a real session: the tab in front moves, a tab
/// leaves its window for one of its own, and an app opens where the person is.
#[test]
fn the_desk_answers_its_shortcuts_inside_the_session() {
    let mut session = session_with(desk_tree());
    let in_front = |session: &Session| {
        session
            .handle()
            .snapshot("session", vec![])
            .active
            .and_then(|route| route.tab.parse::<u64>().ok())
    };
    assert_eq!(in_front(&session), Some(20));

    press(&mut session, ShortcutAction::NextTab);
    until(&mut session, "the next tab", |session| {
        in_front(session) == Some(21)
    });
    press(&mut session, ShortcutAction::PreviousTab);
    until(&mut session, "the tab before", |session| {
        in_front(session) == Some(20)
    });

    press(&mut session, ShortcutAction::DetachTab);
    until(&mut session, "the split", |session| {
        session
            .handle()
            .read(|tree| tree.workspaces[0].strips[0].window_groups.len() == 3)
    });

    press(&mut session, ShortcutAction::OpenFiles);
    until(&mut session, "the files app", |session| {
        apps(session)
            .iter()
            .any(|app| app == "so.tilo.file-explorer")
    });
}
