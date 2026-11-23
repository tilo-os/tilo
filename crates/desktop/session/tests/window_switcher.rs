//! Alt+Tab on the tilolang shell, driven by the compositor's own keys: holding Alt over a Tab
//! walks the windows of the workspace in front, in the order Alt+Tab walks them, and the walk
//! crosses the strips of that workspace and no other. The shell draws the walk once it outlasts a
//! flick, as a panel of the windows it crosses in its order: the window the walk reached is the
//! lifted card, ringed in the person's accent, with the keys the walk answers to under them.
//!
//! Shift walks the other way, the key above Tab narrows the walk to the app in front's windows,
//! Home and End choose by place, Escape leaves the person where they were, letting Alt go goes to
//! the window the walk reached, and a press on a card goes to that window.
//!
//! A flick shorter than the panel's reveal delay switches windows and flashes nothing: the walk
//! is the session's from the press of the chord to the release of Alt. The shell shows what the
//! session publishes of it (`desktop:overlays` for the surface up and the walk's scope,
//! `session:windows` for the windows in the walk's order, the one it reached marked `focused`),
//! and draws its cards from that reading; these tests read both, and the cards as drawn.

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

use std::time::{Duration, Instant};

use crate::common::tree::window;
use crate::common::{BTN_LEFT, HEIGHT, WIDTH, changed, shot, until};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::{Options, Session};
use ui::kurbo::Rect;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_GRAVE: u32 = 41;
pub(crate) const KEY_LEFTALT: u32 = 56;
const KEY_HOME: u32 = 102;
const KEY_END: u32 = 107;

/// One frame at the display's refresh.
const REFRESH: Duration = Duration::from_millis(1);

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

/// The shell the session starts on: two strips and three windows in the workspace in front — two of
/// the notes app, one of the mail app — and a window of another workspace, which the walk never
/// crosses.
fn layout_tree() -> Layout {
    let mut work = Workspace::new(WorkspaceId::from_u128(1), "Work");
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
    let mut elsewhere = Workspace::new(WorkspaceId::from_u128(2), "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![group(13, window(23, "Away", "so.tilo.mail"))],
    }];
    let mut tree = Layout {
        active: work.id(),
        workspaces: vec![work, elsewhere],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 10);
    tree
}

/// The session on the shell above, the tilolang shell as the whole screen.
pub(crate) fn session() -> Session {
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
    session.handle().replace_tree(layout_tree());
    session.step(REFRESH).expect("a turn of the session");
    session
}

pub(crate) fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key reaches the session");
    session.step(REFRESH).expect("a turn of the session");
}

/// The title of the window in front.
pub(crate) fn front(session: &Session) -> String {
    crate::common::windows(session)
        .into_iter()
        .find(|window| window.focused)
        .expect("a window is in front")
        .title
}

/// What the shell shows of the switcher's panel while it is up: the windows Alt+Tab walks, in its
/// order, the one the walk has reached, and whether the walk crosses the app in front's windows
/// alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Panel {
    cards: Vec<String>,
    /// Which card the walk reached, an index into `cards`.
    selected: usize,
    /// The walk crosses the front app's windows (Alt+Grave) rather than the workspace's.
    app_scope: bool,
}

/// What the panel shows now, when it is up. The session publishes the surface it shows
/// (`desktop:overlays`: `here`, `switcher_revealed`, `switcher_scope`) and the windows in the
/// walk's order, the one the walk reached marked `focused` (`session:windows`); the walk crosses
/// the workspace in front, or in `app` scope the windows of the app in front, which is the app of
/// the window the walk lifts.
pub(crate) fn panel(session: &Session) -> Option<Panel> {
    let overlays = session.read("desktop:overlays")?;
    if overlays.get("here")?.as_str()? != "switcher"
        || !overlays.get("switcher_revealed")?.as_bool()?
    {
        return None;
    }
    let app_scope = overlays.get("switcher_scope")?.as_str()? == "app";
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
    })?;
    let windows: Vec<_> = crate::common::windows(session)
        .into_iter()
        .filter(|window| window.workspace_key == workspace)
        .collect();
    let front_app = windows
        .iter()
        .find(|window| window.focused)
        .map(|window| window.app_key.clone());
    let cards: Vec<_> = windows
        .into_iter()
        .filter(|window| !app_scope || front_app.as_deref() == Some(window.app_key.as_str()))
        .collect();
    let selected = cards.iter().position(|window| window.focused)?;
    Some(Panel {
        cards: cards.into_iter().map(|window| window.title).collect(),
        selected,
        app_scope,
    })
}

/// The titles of the panel's cards, in its order.
pub(crate) fn cards(panel: &Panel) -> Vec<String> {
    panel.cards.clone()
}

/// Alt+Tab held down from a fresh shell, and the time until its panel shows.
pub(crate) fn hold_alt_tab(session: &mut Session) -> Duration {
    key(session, KEY_LEFTALT, true);
    let pressed = Instant::now();
    key(session, KEY_TAB, true);
    until(session, "the panel shows while Alt is held", |session| {
        panel(session).is_some()
    });
    pressed.elapsed()
}

/// A node the shell lays out for assistive technology: what it is, what it says, and where it
/// stands, logical `(x0, y0, x1, y1)`: every window's nodes, the shell's among them.
#[derive(Clone, Debug, PartialEq)]
struct Node {
    role: String,
    label: Option<String>,
    bounds: Option<[f64; 4]>,
}

/// The whole laid-out tree, in place: every window's node, as assistive technology reads it from
/// the session's own export.
fn laid_out(session: &Session) -> Vec<Node> {
    let mut out = Vec::new();
    for (_, _, tree) in session.a11y_windows() {
        for (_, node) in tree.nodes {
            out.push(Node {
                role: format!("{:?}", node.role()),
                label: node.label().map(str::to_owned),
                bounds: node
                    .bounds()
                    .map(|rect| [rect.x0, rect.y0, rect.x1, rect.y1]),
            });
        }
    }
    out
}

/// Where the shell draws the switcher's card titled `title` : the smallest control it lays out
/// around the title's own words, as each card carries its window's name over its picture.
fn drawn_card(session: &Session, nodes: &[Node], title: &str) -> Option<Rect> {
    let named = session.shell_controls(title);
    nodes
        .iter()
        .filter(|node| node.role == "Button")
        .filter_map(|node| node.bounds)
        .filter(|[x0, y0, x1, y1]| {
            named.iter().any(|(x, y, w, h)| {
                *x0 <= *x && *y0 <= *y && x + w <= *x1 && y + h <= *y1 && (x1 - x0) > w + 1.0
            })
        })
        .min_by(|a, b| area(a).total_cmp(&area(b)))
        .map(|[x0, y0, x1, y1]| Rect::new(x0, y0, x1, y1))
}

fn area([x0, y0, x1, y1]: &[f64; 4]) -> f64 {
    (x1 - x0) * (y1 - y0)
}

/// Where the panel's cards stand in the shell's laid-out tree now: each card's place in the panel
/// and its bounds, logical `(x0, y0, x1, y1)`. A page of too many windows draws fewer cards than
/// the walk crosses, so only the drawn ones are here.
fn page_of(session: &Session) -> Vec<(usize, Rect)> {
    let Some(panel) = panel(session) else {
        return Vec::new();
    };
    let nodes = laid_out(session);
    panel
        .cards
        .iter()
        .enumerate()
        .filter_map(|(at, title)| drawn_card(session, &nodes, title).map(|rect| (at, rect)))
        .collect()
}

/// Run frames until the shell has drawn the card the walk reached, and with it the page of cards
/// it shows now. The card the walk reached is always on the page, so a page drawn for the walk
/// before the one the session publishes carries no card for it.
fn until_drawn(session: &mut Session) {
    until(session, "the panel draws the cards it shows", |session| {
        let Some(panel) = panel(session) else {
            return false;
        };
        page_of(session).iter().any(|(at, _)| *at == panel.selected)
    });
}

/// Where the panel's cards stand, once the shell has drawn what it shows now.
fn drawn_cards(session: &mut Session) -> Vec<(usize, Rect)> {
    until_drawn(session);
    page_of(session)
}

/// Run a couple of frames after the panel is drawn, so a capture holds the frame the shell drew
/// rather than the one standing before it.
fn settle(session: &mut Session) {
    for _ in 0..2 {
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Move the pointer to the centre of card `index` , once the panel has drawn it where it shows it
/// now.
pub(crate) fn move_to_card(session: &mut Session, index: usize) {
    until(
        session,
        &format!("the panel draws card {index}"),
        |session| page_of(session).iter().any(|(at, _)| *at == index),
    );
    let cards = page_of(session);
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

tilo_test_env::scenario! {
    fn the_switcher_walks_the_workspaces_windows;

    phase letting_alt_go_goes_to_the_window_the_walk_reached;
    // Letting Alt go goes to the window the walk reached, and the panel comes down.
    {
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
        assert_eq!(
            crate::common::shell_here(&session),
            "none",
            "the shell is at rest again"
        );
        shot(&mut session, "switcher-after");
        eprintln!("letting Alt go went to Mail {settled:?} later");
    }

    phase a_flick_shows_no_panel_and_switches_windows;
    // A flick shorter than the reveal delay switches windows and flashes nothing.
    {
        let _settled = ui::motion::testing::settled();
        let mut session = session();
        session
            .step(Duration::from_millis(300))
            .expect("the shell settles");
        let plain = shot(&mut session, "switcher-flick-before");
        key(&mut session, KEY_LEFTALT, true);
        key(&mut session, KEY_TAB, true);
        assert!(
            panel(&session).is_none(),
            "a flick shorter than the reveal delay shows no panel"
        );
        let mid_flick = shot(&mut session, "switcher-flick-mid");
        // The shell is the shell: the walk of a flick crosses the windows with no panel over them.
        let moved = changed(&plain, &mid_flick);
        assert!(
            moved < plain.rgba.len() / 4 / 100,
            "a flick draws no panel over the shell: {moved} pixels differ"
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

    phase one_window_opens_no_walk;
    // Alt+Tab with one window open does nothing: there is nowhere to walk, and no panel stands over
    // the shell.
    {
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
        assert_eq!(crate::common::windows(&session).len(), 1);

        key(&mut session, KEY_LEFTALT, true);
        key(&mut session, KEY_TAB, true);
        session
            .step(Duration::from_millis(200))
            .expect("a turn of the session");
        assert!(panel(&session).is_none(), "one window is nowhere to walk");
        assert_eq!(crate::common::shell_here(&session), "none");
        key(&mut session, KEY_LEFTALT, false);
    }

    phase holding_alt_tab_shows_the_workspaces_windows_across_its_strips;
    // Holding Alt+Tab shows the panel over the windows of the workspace in front, in the walk's order,
    // across its strips and no other workspace's windows; Shift walks the other way and Escape leaves
    // the person where they were.
    {
        let _settled = ui::motion::testing::settled();
        let mut session = session();
        settle(&mut session);
        let plain = shot(&mut session, "switcher-shell");

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
            "the shell lifts the window the walk reached, as the shell's reading always has"
        );
        until_drawn(&mut session);
        settle(&mut session);
        let held = shot(&mut session, "switcher-held");
        assert!(
            changed(&plain, &held) > plain.rgba.len() / 4 / 100,
            "the panel shows over the shell: {} pixels differ",
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
    until_drawn(&mut session);
    settle(&mut session);
    let app = shot(&mut session, "switcher-app-scope");

    // Home chooses the window in front, End the last of them.
    key(&mut session, KEY_HOME, true);
    key(&mut session, KEY_HOME, false);
    assert_eq!(
        panel(&session).expect("up").selected,
        0,
        "Home chooses the first"
    );
    until_drawn(&mut session);
    settle(&mut session);
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
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(5));
    tree.active_group.insert(StripId(5), 100);
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(crate::common::windows(&session).len(), 14);

    hold_alt_tab(&mut session);
    let page = drawn_cards(&mut session);
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
    let last = drawn_cards(&mut session);
    assert!(
        last.iter().any(|(at, _)| *at == 13),
        "the page turned to the window the walk reached: {last:?}"
    );
    assert_ne!(
        page.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        last.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        "the last window's page is another page"
    );
    settle(&mut session);
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
    until_drawn(&mut session);
    let read: Vec<String> = laid_out(&session)
        .into_iter()
        .filter(|node| node.role == "StaticText")
        .filter_map(|node| node.label)
        .collect();
    assert!(
        read.iter()
            .any(|said| said == "Alt + Tab: Walk, Enter: Go there, Esc: Stay"),
        "the walk's keys, in the person's words, are read under its windows: {read:?}"
    );
}

/// The card the walk reached wears the person's colour at its edge, and the cards it passed do not:
/// which window Alt+Tab would go to reads at a glance, whatever the theme.
#[test]
fn the_walked_card_wears_the_accent_at_its_edge() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    hold_alt_tab(&mut session);
    until_drawn(&mut session);
    settle(&mut session);
    let view = panel(&session).expect("the panel is up");
    // The cards where the shell drew them, and the frame it drew: the same pass, so a card's
    // bounds are where its edge's pixels are.
    let cards = page_of(&session);
    let frame = shot(&mut session, "switcher-ring");
    // The shell draws the reached card's edge in the person's accent of the theme it draws with:
    // these sessions draw in Tilo dark, as the headless runner's inputs say.
    let ring = ui::theme::Theme::dark()
        .tokens
        .colors
        .intent
        .accent
        .to_rgba8();
    let ring = [ring.r, ring.g, ring.b];
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

/// Alt+Tab in an empty active workspace opens nothing and never selects a window in another
/// workspace, even when another workspace holds two or more windows.
#[test]
fn an_empty_active_workspace_never_walks_to_another_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let empty_id = WorkspaceId::from_u128(1);
    let elsewhere_id = WorkspaceId::from_u128(2);

    for with_empty_strip in [false, true] {
        let mut empty = Workspace::new(empty_id, "Empty");
        if with_empty_strip {
            empty.strips = vec![Strip {
                id: StripId(2),
                name: "Main".into(),
                window_groups: Vec::new(),
            }];
        }
        let mut elsewhere = Workspace::new(elsewhere_id, "Elsewhere");
        elsewhere.strips = vec![Strip {
            id: StripId(4),
            name: "Main".into(),
            window_groups: vec![
                group(13, window(23, "Away", "so.tilo.mail")),
                group(14, window(24, "Far", "so.tilo.notes")),
            ],
        }];
        let mut tree = Layout {
            active: empty_id,
            workspaces: vec![empty, elsewhere],
            ..Default::default()
        };
        if with_empty_strip {
            tree.active_strip.insert(empty_id, StripId(2));
        }
        handle.replace_tree(tree);
        session.step(REFRESH).expect("a turn of the session");
        assert_eq!(crate::common::windows(&session).len(), 2);

        key(&mut session, KEY_LEFTALT, true);
        key(&mut session, KEY_TAB, true);
        session
            .step(Duration::from_millis(200))
            .expect("a turn of the session");
        assert!(
            panel(&session).is_none(),
            "with_empty_strip={with_empty_strip}: an empty active workspace opens no panel"
        );
        assert_eq!(
            crate::common::shell_here(&session),
            "none",
            "with_empty_strip={with_empty_strip}: an empty active workspace starts no walk"
        );
        key(&mut session, KEY_TAB, false);
        key(&mut session, KEY_LEFTALT, false);
        session.step(REFRESH).expect("a turn of the session");
        assert_eq!(
            handle.read(|tree| tree.active),
            empty_id,
            "with_empty_strip={with_empty_strip}: Alt+Tab never leaves an empty active workspace"
        );
    }
}

/// A workspace whose recorded strip no longer exists falls back to its first strip for focus and
/// stays inside that workspace when Alt+Tab walks its windows.
#[test]
fn a_stale_recorded_strip_keeps_the_walk_inside_its_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let mut tree = layout_tree();
    // Record a strip that no longer exists in Work while Work still holds its strips and
    // Elsewhere holds another window.
    tree.active_strip.insert(work_id, StripId(99));
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");

    assert_eq!(
        handle.read(|tree| tree.active_strip.get(&work_id).copied()),
        Some(StripId(99)),
        "the stale recorded strip is still in the layout when the walk begins"
    );
    assert_eq!(
        front(&session),
        "Notes",
        "a stale recorded strip falls back to the workspace's first strip for focus"
    );

    hold_alt_tab(&mut session);
    let view = panel(&session).expect("the panel is up while Alt is held");
    assert_eq!(
        cards(&view),
        ["Notes", "Mail", "Budget"],
        "the walk stays inside the active workspace and omits other workspaces' windows"
    );
    assert_eq!(
        view.selected, 1,
        "Alt+Tab opens on the window after the fallback strip's focused window"
    );

    key(&mut session, KEY_END, true);
    key(&mut session, KEY_END, false);
    assert_eq!(
        panel(&session).expect("the panel is up").selected,
        2,
        "End reaches the active workspace's last window, never another workspace's"
    );
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "letting Alt go lands on the active workspace's last window",
        |session| front(session) == "Budget",
    );
    assert_eq!(
        handle.read(|tree| tree.active),
        work_id,
        "Alt+Tab stayed in the active workspace"
    );
}

/// With two windows in the active workspace and none in front (for example when the active strip
/// is empty and the windows live on another strip), the first Alt+Tab reaches the first window
/// rather than skipping it, and subsequent steps walk on from there.
#[test]
fn alt_tab_with_two_windows_and_none_in_front_reaches_the_first() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let elsewhere_id = WorkspaceId::from_u128(2);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: Vec::new(),
        },
        Strip {
            id: StripId(3),
            name: "Side".into(),
            window_groups: vec![
                group(10, window(20, "Notes", "so.tilo.notes")),
                group(11, window(21, "Mail", "so.tilo.mail")),
            ],
        },
    ];
    let mut elsewhere = Workspace::new(elsewhere_id, "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![group(13, window(23, "Away", "so.tilo.mail"))],
    }];
    let mut tree = Layout {
        active: work_id,
        workspaces: vec![work, elsewhere],
        ..Default::default()
    };
    tree.active_strip.insert(work_id, StripId(2));
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        crate::common::windows(&session)
            .iter()
            .all(|window| !window.focused),
        "no window is in front on the empty active strip"
    );

    hold_alt_tab(&mut session);
    let view = panel(&session).expect("the panel is up while Alt is held");
    assert_eq!(cards(&view), ["Notes", "Mail"]);
    assert_eq!(
        view.selected, 0,
        "with no window in front the first Alt+Tab lands on the first window"
    );
    assert_eq!(front(&session), "Notes");

    // Stepping on reaches the second window; Home and End choose by place.
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_TAB, true);
    assert_eq!(panel(&session).expect("the panel is up").selected, 1);
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_HOME, true);
    key(&mut session, KEY_HOME, false);
    assert_eq!(panel(&session).expect("the panel is up").selected, 0);
    key(&mut session, KEY_END, true);
    key(&mut session, KEY_END, false);
    assert_eq!(panel(&session).expect("the panel is up").selected, 1);
    key(&mut session, KEY_LEFTSHIFT, true);
    key(&mut session, KEY_TAB, true);
    assert_eq!(panel(&session).expect("the panel is up").selected, 0);
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTSHIFT, false);
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "letting Alt go brings the first window to the front",
        |session| front(session) == "Notes",
    );
    assert_eq!(handle.read(|tree| tree.active), work_id);
}

/// With windows in the active workspace and none in front, the first Alt+Shift+Tab lands on the
/// last window of the walk rather than skipping it, and stepping backward walks from there.
#[test]
fn alt_shift_tab_with_none_in_front_reaches_the_last_window() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let elsewhere_id = WorkspaceId::from_u128(2);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: Vec::new(),
        },
        Strip {
            id: StripId(3),
            name: "Side".into(),
            window_groups: vec![
                group(10, window(20, "Notes", "so.tilo.notes")),
                group(11, window(21, "Mail", "so.tilo.mail")),
                group(12, window(22, "Budget", "so.tilo.notes")),
            ],
        },
    ];
    let mut elsewhere = Workspace::new(elsewhere_id, "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![group(13, window(23, "Away", "so.tilo.mail"))],
    }];
    let mut tree = Layout {
        active: work_id,
        workspaces: vec![work, elsewhere],
        ..Default::default()
    };
    tree.active_strip.insert(work_id, StripId(2));
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        crate::common::windows(&session)
            .iter()
            .all(|window| !window.focused),
        "no window is in front on the empty active strip"
    );

    key(&mut session, KEY_LEFTALT, true);
    key(&mut session, KEY_LEFTSHIFT, true);
    key(&mut session, KEY_TAB, true);
    until(
        &mut session,
        "the panel shows while Alt+Shift+Tab is held",
        |session| panel(session).is_some(),
    );
    let view = panel(&session).expect("the panel is up while Alt is held");
    assert_eq!(cards(&view), ["Notes", "Mail", "Budget"]);
    assert_eq!(
        view.selected, 2,
        "with no window in front the first Alt+Shift+Tab lands on the last window"
    );
    assert_eq!(front(&session), "Budget");

    // A second backward step walks to the second-to-last window.
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_TAB, true);
    assert_eq!(panel(&session).expect("the panel is up").selected, 1);
    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTSHIFT, false);
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "letting Alt go brings the reached window to the front",
        |session| front(session) == "Mail",
    );
    assert_eq!(handle.read(|tree| tree.active), work_id);
}

/// With a lone window in the active workspace that is not in front (for example on another strip
/// while the active strip is empty), Alt+Tab opens and reaches that window.
#[test]
fn alt_tab_with_one_window_and_none_in_front_reaches_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let elsewhere_id = WorkspaceId::from_u128(2);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: Vec::new(),
        },
        Strip {
            id: StripId(3),
            name: "Side".into(),
            window_groups: vec![group(12, window(22, "Budget", "so.tilo.notes"))],
        },
    ];
    let mut elsewhere = Workspace::new(elsewhere_id, "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![group(13, window(23, "Away", "so.tilo.mail"))],
    }];
    let mut tree = Layout {
        active: work_id,
        workspaces: vec![work, elsewhere],
        ..Default::default()
    };
    tree.active_strip.insert(work_id, StripId(2));
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert!(
        crate::common::windows(&session)
            .iter()
            .all(|window| !window.focused),
        "the lone window on the other strip is not in front"
    );

    hold_alt_tab(&mut session);
    let view = panel(&session).expect("the panel is up while Alt is held");
    assert_eq!(cards(&view), ["Budget"]);
    assert_eq!(view.selected, 0);

    key(&mut session, KEY_TAB, false);
    key(&mut session, KEY_LEFTALT, false);
    until(
        &mut session,
        "letting Alt go focuses the lone window",
        |session| front(session) == "Budget",
    );
    assert_eq!(handle.read(|tree| tree.active), work_id);
}

/// Invoking `show-surface` or `open-surface` for `switcher` in an empty active workspace leaves the
/// switcher down, shows no panel and holds no keys.
#[test]
fn show_surface_switcher_does_not_open_on_an_empty_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let empty_id = WorkspaceId::from_u128(1);
    let elsewhere_id = WorkspaceId::from_u128(2);
    let empty = Workspace::new(empty_id, "Empty");
    let mut elsewhere = Workspace::new(elsewhere_id, "Elsewhere");
    elsewhere.strips = vec![Strip {
        id: StripId(4),
        name: "Main".into(),
        window_groups: vec![
            group(13, window(23, "Away", "so.tilo.mail")),
            group(14, window(24, "Far", "so.tilo.notes")),
        ],
    }];
    handle.replace_tree(Layout {
        active: empty_id,
        workspaces: vec![empty, elsewhere],
        ..Default::default()
    });
    session.step(REFRESH).expect("a turn of the session");

    let show_args = serde_json::Map::from_iter([(
        "surface".to_owned(),
        serde_json::Value::String("switcher".to_owned()),
    )]);
    session
        .invoke("desktop:overlays", "show-surface", &show_args)
        .expect("show-surface succeeds");
    session
        .step(Duration::from_millis(200))
        .expect("a turn of the session");
    assert_eq!(
        crate::common::shell_here(&session),
        "none",
        "show-surface must not leave the switcher up on an empty workspace"
    );
    assert!(panel(&session).is_none());

    let open_args = serde_json::Map::from_iter([(
        "part".to_owned(),
        serde_json::Value::String("switcher".to_owned()),
    )]);
    session
        .invoke("desktop:overlays", "open-surface", &open_args)
        .expect("open-surface succeeds");
    session
        .step(Duration::from_millis(200))
        .expect("a turn of the session");
    assert_eq!(
        crate::common::shell_here(&session),
        "none",
        "open-surface must not leave the switcher up on an empty workspace"
    );
    assert!(panel(&session).is_none());
    assert_eq!(handle.read(|tree| tree.active), empty_id);
}

/// Invoking `show-surface` or `open-surface` for `switcher` when the active workspace holds one
/// focused window opens the switcher with that one card: only an empty walk is refused.
#[test]
fn show_surface_switcher_opens_with_one_focused_window() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group(10, window(20, "Notes", "so.tilo.notes"))],
    }];
    let mut tree = Layout {
        active: work_id,
        workspaces: vec![work],
        ..Default::default()
    };
    tree.active_strip.insert(work_id, StripId(2));
    tree.active_group.insert(StripId(2), 10);
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(front(&session), "Notes");

    let show_args = serde_json::Map::from_iter([(
        "surface".to_owned(),
        serde_json::Value::String("switcher".to_owned()),
    )]);
    session
        .invoke("desktop:overlays", "show-surface", &show_args)
        .expect("show-surface succeeds");
    until(
        &mut session,
        "show-surface opens the switcher on a lone focused window",
        |session| panel(session).is_some(),
    );
    assert_eq!(crate::common::shell_here(&session), "switcher");
    let view = panel(&session).expect("the panel is up");
    assert_eq!(cards(&view), ["Notes"]);
    assert_eq!(view.selected, 0);

    // Toggling show-surface again takes the switcher back down.
    session
        .invoke("desktop:overlays", "show-surface", &show_args)
        .expect("show-surface toggles off");
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(crate::common::shell_here(&session), "none");
    assert!(panel(&session).is_none());

    // open-surface behaves the same way on a lone focused window.
    let open_args = serde_json::Map::from_iter([(
        "part".to_owned(),
        serde_json::Value::String("switcher".to_owned()),
    )]);
    session
        .invoke("desktop:overlays", "open-surface", &open_args)
        .expect("open-surface succeeds");
    until(
        &mut session,
        "open-surface opens the switcher on a lone focused window",
        |session| panel(session).is_some(),
    );
    assert_eq!(crate::common::shell_here(&session), "switcher");
    let view = panel(&session).expect("the panel is up");
    assert_eq!(cards(&view), ["Notes"]);
    assert_eq!(view.selected, 0);
}

/// Switching an open workspace walk to `app` scope via `walk-switcher` when no window is in front
/// produces an empty walk and takes the switcher down immediately instead of holding the keyboard.
#[test]
fn walk_switcher_in_app_scope_closes_when_no_window_is_in_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let handle = session.handle().clone();
    let work_id = WorkspaceId::from_u128(1);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: Vec::new(),
        },
        Strip {
            id: StripId(3),
            name: "Side".into(),
            window_groups: vec![
                group(10, window(20, "Notes", "so.tilo.notes")),
                group(11, window(21, "Mail", "so.tilo.mail")),
            ],
        },
    ];
    let mut tree = Layout {
        active: work_id,
        workspaces: vec![work],
        ..Default::default()
    };
    tree.active_strip.insert(work_id, StripId(2));
    handle.replace_tree(tree);
    session.step(REFRESH).expect("a turn of the session");

    let walk_args = |scope: &str| {
        serde_json::Map::from_iter([
            ("steps".to_owned(), serde_json::Value::from(1)),
            (
                "scope".to_owned(),
                serde_json::Value::String(scope.to_owned()),
            ),
        ])
    };
    session
        .invoke("desktop:overlays", "walk-switcher", &walk_args("workspace"))
        .expect("workspace walk opens");
    until(&mut session, "workspace walk panel reveals", |session| {
        panel(session).is_some()
    });
    assert_eq!(crate::common::shell_here(&session), "switcher");

    session
        .invoke("desktop:overlays", "walk-switcher", &walk_args("app"))
        .expect("app-scope walk runs");
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(
        crate::common::shell_here(&session),
        "none",
        "an empty app-scope walk closes the switcher"
    );
    assert!(panel(&session).is_none());
    // Keys are no longer held by the switcher: pressing Enter commits no window.
    key(&mut session, KEY_ENTER, true);
    key(&mut session, KEY_ENTER, false);
    assert!(
        crate::common::windows(&session)
            .iter()
            .all(|window| !window.focused),
        "Enter after the switcher closed does not commit a window"
    );

    // An open walk whose workspace empties even before the reveal delay elapses closes on the
    // next pass and does not stay up holding keys.
    session
        .invoke("desktop:overlays", "walk-switcher", &walk_args("workspace"))
        .expect("workspace walk opens before reveal");
    assert_eq!(crate::common::shell_here(&session), "switcher");
    assert!(panel(&session).is_none(), "panel has not revealed yet");
    let mut emptied = handle.read(Clone::clone);
    for strip in &mut emptied.workspaces[0].strips {
        strip.window_groups.clear();
    }
    handle.replace_tree(emptied);
    session.step(REFRESH).expect("a turn of the session");
    assert_eq!(
        crate::common::shell_here(&session),
        "none",
        "an emptied walk closes immediately even before the reveal delay"
    );
    assert!(panel(&session).is_none());
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
