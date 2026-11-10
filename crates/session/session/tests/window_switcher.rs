//! Alt+Tab on the default Rust session, driven by the compositor's own keys: holding Alt over
//! a Tab puts the chrome's panel up over the windows of the workspace in front, in the order
//! Alt+Tab walks them, and the walk crosses the strips of that workspace and no other. Shift
//! walks the other way, the key above Tab narrows the walk to the app in front's windows,
//! Home and End choose by place, Escape leaves the person where they were, letting Alt go goes
//! to the window the walk reached, and a press on a card goes to that window.
//!
//! A flick shorter than the panel's reveal delay switches windows and flashes nothing: the
//! walk is the desk's from the press of the chord to the release of Alt.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::{Duration, Instant};

use common::{BTN_LEFT, HEIGHT, WIDTH, changed, shot, until};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_desktop::window_switcher::SwitcherView;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_session::{Options, Session};
use tilo_session_core::shell::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;
use ui::kurbo::Rect;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_GRAVE: u32 = 41;
const KEY_LEFTALT: u32 = 56;
const KEY_HOME: u32 = 102;
const KEY_END: u32 = 107;

/// One frame at the display's refresh.
const REFRESH: Duration = Duration::from_millis(1);

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

fn group(id: u64, tab: Tab) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tab.id,
        tabs: vec![tab],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// The desk the session starts on: two strips and three windows in the workspace in front — two of
/// the notes app, one of the mail app — and a window of another workspace, which the walk never
/// crosses.
fn desk_tree() -> DesktopTree {
    let mut work = Workspace::new(WorkspaceSlot(1), "Work");
    work.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![
                group(10, window(20, "Notes", "so.tilo.notes")),
                group(11, window(21, "Mail", "so.tilo.mail")),
            ],
        },
        Strip {
            id: StripId(3),
            name: "Side".into(),
            window_groups: vec![group(12, window(22, "Budget", "so.tilo.notes"))],
        },
    ];
    let mut elsewhere = Workspace::new(WorkspaceSlot(2), "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![group(13, window(23, "Away", "so.tilo.mail"))],
    }];
    let mut tree = DesktopTree {
        active: work.slot,
        workspaces: vec![work, elsewhere],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 10);
    tree
}

/// The default session — the Rust chrome, not the opt-in shell — on the desk above.
fn session() -> Session {
    session_on(WIDTH, HEIGHT)
}

/// [`session`] drawing on a display `width` by `height` : a small one holds fewer cards.
fn session_on(width: i32, height: i32) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height)],
        Options {
            collab: false,
            ..Options::default()
        },
    );
    // A held Tab repeats on a desktop; here a key is pressed once, so a slow step does not turn one
    // press into several.
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session.handle().replace_tree(desk_tree());
    session.step(REFRESH).expect("a turn of the session");
    session
}

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key reaches the session");
    session.step(REFRESH).expect("a turn of the session");
}

/// The desk as the session publishes it.
fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(
        session
            .read(DESK_RESOURCE)
            .expect("the session publishes its desk"),
    )
    .expect("the desk reads as the desk's reading")
}

/// The title of the window in front.
fn front(session: &Session) -> String {
    desk(session)
        .windows
        .into_iter()
        .find(|window| window.focused)
        .expect("a window is in front")
        .title
}

/// What the panel shows now, when it is up.
fn panel(session: &Session) -> Option<SwitcherView> {
    session.window_switcher().view()
}

/// The titles of the panel's cards, in its order.
fn cards(view: &SwitcherView) -> Vec<String> {
    view.cards.iter().map(|card| card.title.clone()).collect()
}

/// Alt+Tab held down from a fresh desk, and the time until its panel shows.
fn hold_alt_tab(session: &mut Session) -> Duration {
    key(session, KEY_LEFTALT, true);
    let pressed = Instant::now();
    key(session, KEY_TAB, true);
    until(session, "the panel shows while Alt is held", |session| {
        panel(session).is_some()
    });
    pressed.elapsed()
}

/// Move the pointer to the centre of card `index` , as the panel was last drawn.
fn move_to_card(session: &mut Session, index: usize) {
    let cards = session.window_switcher().card_rects();
    let (_, card) = cards
        .iter()
        .find(|(at, _)| *at == index)
        .expect("the panel drew that card");
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::midpoint(card.x0, card.x1),
            y: f64::midpoint(card.y0, card.y1),
        })
        .expect("the pointer moves");
}

/// Holding Alt+Tab shows the panel over the windows of the workspace in front, in the walk's order,
/// across its strips and no other workspace's windows; Shift walks the other way and Escape leaves
/// the person where they were.
#[test]
fn holding_alt_tab_shows_the_workspaces_windows_across_its_strips() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let plain = shot(&mut session, "switcher-desk");

    let waited = hold_alt_tab(&mut session);
    let view = panel(&session).expect("the panel is up while Alt is held");
    assert_eq!(
        cards(&view),
        ["Notes", "Mail", "Budget"],
        "the walk crosses the workspace in front, its strips in order, from the window in front"
    );
    assert_eq!(
        view.selected, 1,
        "Alt+Tab opens on the window before the one in front"
    );
    assert!(!view.app_scope);
    assert_eq!(
        front(&session),
        "Mail",
        "the desk lifts the window the walk reached, as the shell's reading always has"
    );
    let held = shot(&mut session, "switcher-held");
    assert!(
        changed(&plain, &held) > plain.rgba.len() / 4 / 100,
        "the panel shows over the desk: {} pixels differ",
        changed(&plain, &held)
    );

    // Releasing Tab is no answer: the walk stays up.
    key(&mut session, KEY_TAB, false);
    assert!(
        panel(&session).is_some(),
        "Tab released with Alt down leaves the panel up"
    );

    // Shift walks the other way.
    key(&mut session, KEY_LEFTSHIFT, true);
    key(&mut session, KEY_TAB, true);
    assert_eq!(
        panel(&session).expect("the panel is up").selected,
        0,
        "Shift+Tab walks back from the window before the one in front, to it"
    );
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTSHIFT, false);

    // Escape takes it down and leaves the person where they were.
    key(&mut session, KEY_ESCAPE, true);
    key(&mut session, KEY_ESCAPE, false);
    until(&mut session, "Escape takes the panel down", |session| {
        panel(session).is_none()
    });
    key(&mut session, KEY_LEFTALT, false);
    assert_eq!(front(&session), "Notes", "Escape switched nothing");

    eprintln!("the panel showed {waited:?} after Alt+Tab was pressed");
}

/// Letting Alt go goes to the window the walk reached, and the panel comes down.
#[test]
fn letting_alt_go_goes_to_the_window_the_walk_reached() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    hold_alt_tab(&mut session);
    assert_eq!(panel(&session).expect("the panel is up").selected, 1);

    let released = Instant::now();
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "Alt let go brings the walk's window front",
        |session| front(session) == "Mail",
    );
    let settled = released.elapsed();
    assert!(
        panel(&session).is_none(),
        "the panel is down once Alt is let go"
    );
    assert_eq!(desk(&session).here, "none", "the desk is at rest again");
    shot(&mut session, "switcher-after");
    eprintln!("letting Alt go went to Mail {settled:?} later");
}

/// The key above Tab walks only the windows of the app in front, across strips; Home and End choose
/// the first and last of them by place, and Enter goes to the chosen one.
#[test]
fn alt_grave_walks_only_the_front_apps_windows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    key(&mut session, KEY_LEFTALT, true);
    key(&mut session, KEY_GRAVE, true);
    until(&mut session, "the app's panel shows", |session| {
        panel(session).is_some()
    });
    let view = panel(&session).expect("the panel is up");
    assert_eq!(
        cards(&view),
        ["Notes", "Budget"],
        "Alt+` crosses the front app's windows, across the strips it holds them on"
    );
    assert!(view.app_scope, "the panel says which walk it shows");
    assert_eq!(view.selected, 1);
    let app = shot(&mut session, "switcher-app-scope");

    // Home chooses the window in front, End the last of them.
    key(&mut session, KEY_HOME, true);
    key(&mut session, KEY_HOME, false);
    assert_eq!(
        panel(&session).expect("up").selected,
        0,
        "Home chooses the first"
    );
    let chosen = shot(&mut session, "switcher-app-scope-first");
    let moved = changed(&app, &chosen);
    assert!(moved > 0, "moving the walk draws another card");
    eprintln!("the walk moving drew {moved} pixels");
    key(&mut session, KEY_END, true);
    key(&mut session, KEY_END, false);
    assert_eq!(
        panel(&session).expect("up").selected,
        1,
        "End chooses the last"
    );

    key(&mut session, KEY_ENTER, true);
    key(&mut session, KEY_ENTER, false);
    until(
        &mut session,
        "Enter goes to the last of the app's windows",
        |session| front(session) == "Budget",
    );
    assert!(panel(&session).is_none());
}

/// A flick shorter than the reveal delay switches windows and flashes nothing.
#[test]
fn a_flick_shows_no_panel_and_switches_windows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session
        .step(Duration::from_millis(300))
        .expect("the desk settles");
    let plain = shot(&mut session, "switcher-flick-before");
    key(&mut session, KEY_LEFTALT, true);
    key(&mut session, KEY_TAB, true);
    assert!(
        panel(&session).is_none(),
        "a flick shorter than the reveal delay shows no panel"
    );
    let mid_flick = shot(&mut session, "switcher-flick-mid");
    // The desk is the desk: the walk of a flick crosses the windows with no panel over them.
    let moved = changed(&plain, &mid_flick);
    assert!(
        moved < plain.rgba.len() / 4 / 100,
        "a flick draws no panel over the desk: {moved} pixels differ"
    );
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "the flick went to the window before the one in front",
        |session| front(session) == "Mail",
    );
    shot(&mut session, "switcher-flick-after");
    // The window just left stands after the one in front, where a held walk opens.
    let waited = hold_alt_tab(&mut session);
    assert_eq!(
        cards(&panel(&session).expect("the panel is up")),
        ["Mail", "Notes", "Budget"]
    );
    eprintln!("the panel showed {waited:?} after Alt+Tab was pressed");
}

/// The card under the pointer is chosen on sight, and a press on it goes to that window.
#[test]
fn a_card_under_the_pointer_is_chosen_on_sight_and_a_press_goes_there() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session
        .step(Duration::from_millis(300))
        .expect("the desk settles");
    hold_alt_tab(&mut session);
    assert_eq!(
        cards(&panel(&session).expect("the panel is up")),
        ["Notes", "Mail", "Budget"]
    );
    move_to_card(&mut session, 2);
    until(
        &mut session,
        "the card under the pointer is chosen on sight",
        |session| front(session) == "Budget",
    );
    // The card the walk reached may stand elsewhere: the pointer goes to where it stands now before
    // the press, as the person's would.
    move_to_card(&mut session, 2);
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("the press reaches the session");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .expect("the pointer lets go");
    until(
        &mut session,
        "a press on a card goes to that window",
        |session| panel(session).is_none() && front(session) == "Budget",
    );
    key(&mut session, KEY_LEFTALT, false);
}

/// A press off the panel is no press on the window behind it: the walk ends where it stood.
#[test]
fn a_press_off_the_panel_ends_the_walk_where_it_stood() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session
        .step(Duration::from_millis(300))
        .expect("the desk settles");
    hold_alt_tab(&mut session);
    move_to_card(&mut session, 1);
    until(&mut session, "the walk reached another window", |session| {
        front(session) == "Mail"
    });
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 5.0, y: 400.0 })
        .expect("the pointer moves off the panel");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("the press reaches the session");
    until(
        &mut session,
        "a press off the panel takes it down",
        |session| panel(session).is_none(),
    );
    key(&mut session, KEY_LEFTALT, false);
    assert_eq!(
        front(&session),
        "Notes",
        "a press outside the panel switches nothing"
    );
}

/// A workspace holding more windows than the display holds cards shows the page the walk has
/// reached, so the card the walk stands on is always on screen.
#[test]
fn a_workspace_too_full_for_cards_shows_the_page_the_walk_reached() {
    const SMALL: (i32, i32) = (640, 400);
    let _settled = ui::motion::testing::settled();
    let mut session = session_on(SMALL.0, SMALL.1);
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let mut workspace = tree.workspaces[0].clone();
    workspace.strips = vec![Strip {
        id: StripId(5),
        name: "Many".into(),
        window_groups: (0..14)
            .map(|at| {
                let id = 100 + at;
                group(
                    id,
                    window(id + 100, &format!("Window {at}"), "so.tilo.notes"),
                )
            })
            .collect(),
    }];
    // This workspace alone: the walk never crosses another's windows, so another workspace here
    // would only be a window the page could not show.
    tree.workspaces = vec![workspace];
    tree.active_strip.insert(WorkspaceSlot(1), StripId(5));
    tree.active_group.insert(StripId(5), 100);
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(desk(&session).windows.len(), 14);

    hold_alt_tab(&mut session);
    let page = session.window_switcher().card_rects();
    assert!(
        page.len() < 14,
        "a page holds fewer cards than the workspace has windows: {} cards",
        page.len()
    );
    assert!(
        page.iter().any(|(at, _)| *at == 0),
        "the walk opens on the window before the one in front, on its page"
    );
    let (screen_w, screen_h) = (f64::from(SMALL.0), f64::from(SMALL.1));
    for (at, card) in &page {
        assert!(
            card.x0 >= 0.0 && card.y0 >= 0.0,
            "card {at} stands on the display: {card:?}"
        );
        assert!(
            card.x1 <= screen_w && card.y1 <= screen_h,
            "card {at} stays inside the display: {card:?}"
        );
    }

    // End walks to the last window: the page turns with it.
    key(&mut session, KEY_END, true);
    key(&mut session, KEY_END, false);
    assert_eq!(
        panel(&session).expect("up").selected,
        13,
        "End chose the last"
    );
    let last = session.window_switcher().card_rects();
    assert!(
        last.iter().any(|(at, _)| *at == 13),
        "the page turned to the window the walk reached: {last:?}"
    );
    assert_ne!(
        page.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        last.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        "the last window's page is another page"
    );
    shot(&mut session, "switcher-last-page");
    key(&mut session, KEY_ENTER, true);
    key(&mut session, KEY_ENTER, false);
    until(&mut session, "Enter goes to the last window", |session| {
        front(session) == "Window 13"
    });
}

/// The keys the walk answers to stand under the cards, in the person's own words, and a reader
/// hears them after the windows: the same hints the panel draws.
#[test]
fn the_panels_keys_are_read_under_its_windows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    hold_alt_tab(&mut session);
    let root = session
        .window_switcher()
        .accessibility()
        .expect("the panel reads");
    let keys = root
        .children
        .iter()
        .find(|child| child.role == ui::a11y::Role::StaticText)
        .expect("the keys are read under the windows");
    assert_eq!(
        keys.name.as_deref(),
        Some("Alt + Tab: Walk, Enter: Go there, Esc: Stay"),
        "the walk's keys, in the person's words"
    );
}

/// The card the walk reached wears the person's colour at its edge, and the cards it passed do not:
/// which window Alt+Tab would go to reads at a glance, whatever the theme.
#[test]
fn the_walked_card_wears_the_accent_at_its_edge() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    hold_alt_tab(&mut session);
    until(&mut session, "the panel draws its cards", |session| {
        !session.window_switcher().card_rects().is_empty()
    });
    let view = panel(&session).expect("the panel is up");
    let frame = shot(&mut session, "switcher-ring");
    let ring = ui::theme::Theme::dark().border_focus.to_rgba8();
    let ring = [ring.r, ring.g, ring.b];
    let cards = session.window_switcher().card_rects();
    let chosen = cards
        .iter()
        .find(|(at, _)| *at == view.selected)
        .expect("the reached card is drawn");
    let nearest = |card: Rect| nearest_edge_pixel(&frame, card, ring);
    assert!(
        nearest(chosen.1) <= 24,
        "the reached card's edge is the person's colour: {ring:?} against {}",
        nearest(chosen.1)
    );
    for (at, card) in &cards {
        if *at == view.selected {
            continue;
        }
        assert!(
            nearest(*card) > 60,
            "card {at} the walk passed is not ringed: {} from {ring:?}",
            nearest(*card)
        );
    }
}

/// How far the frame's pixels along a card's top edge stand from `to` : the least of the rows the
/// edge's ring covers, at three places across it, as summed channel distance.
fn nearest_edge_pixel(frame: &tilo_compositor::Screenshot, card: Rect, to: [u8; 3]) -> u32 {
    let distance = |pixel: [u8; 4]| -> u32 {
        [0, 1, 2]
            .iter()
            .map(|at| u32::from(pixel[*at].abs_diff(to[*at])))
            .sum()
    };
    let ys = (-2..=0).map(|offset| on_display(card.y0) + offset);
    let xs = [0.25, 0.5, 0.75].map(|share| on_display(card.x0 + card.width() * share));
    xs.iter()
        .flat_map(|x| ys.clone().map(move |y| (*x, y)))
        .map(|(x, y)| {
            distance(
                frame
                    .pixel(u32::try_from(x).unwrap_or(0), u32::try_from(y).unwrap_or(0))
                    .unwrap_or([0; 4]),
            )
        })
        .min()
        .unwrap_or(u32::MAX)
}

/// A frame coordinate as the pixel index of the display it stands on.
fn on_display(value: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a place on a display, rounded to its pixel"
    )]
    let pixel = value.round() as i32;
    pixel
}

/// Alt+Tab with one window open does nothing: there is nowhere to walk, and no panel stands over
/// the desk.
#[test]
fn one_window_opens_no_walk() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    tree.workspaces = vec![
        tree.active_workspace()
            .expect("a workspace is in front")
            .clone(),
    ];
    let workspace = &mut tree.workspaces[0];
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group(10, window(20, "Notes", "so.tilo.notes"))],
    }];
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(desk(&session).windows.len(), 1);

    key(&mut session, KEY_LEFTALT, true);
    key(&mut session, KEY_TAB, true);
    session
        .step(Duration::from_millis(200))
        .expect("a turn of the session");
    assert!(panel(&session).is_none(), "one window is nowhere to walk");
    assert_eq!(desk(&session).here, "none");
    key(&mut session, KEY_LEFTALT, false);
}
