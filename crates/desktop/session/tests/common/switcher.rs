//! Alt+Tab on the tilolang shell, as its tests drive it: the shell the session starts on (two
//! strips and three windows in the workspace in front, and a window of another workspace), the keys
//! the compositor sends, and the switcher's panel as the session publishes it (`desktop:overlays`
//! for the surface up and the walk's scope, `session:windows` for the windows in the walk's order)
//! and as the shell draws its cards.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::{Duration, Instant};

use crate::common::tree::window;
use crate::common::{HEIGHT, WIDTH, until};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::{Options, Session};
use ui::kurbo::Rect;

/// What a key sends, evdev codes.
pub(crate) const KEY_TAB: u32 = 15;
pub(crate) const KEY_ENTER: u32 = 28;
pub(crate) const KEY_LEFTSHIFT: u32 = 42;
pub(crate) const KEY_GRAVE: u32 = 41;
pub(crate) const KEY_LEFTALT: u32 = 56;
pub(crate) const KEY_HOME: u32 = 102;
pub(crate) const KEY_END: u32 = 107;

/// One frame at the display's refresh.
pub(crate) const REFRESH: Duration = Duration::from_millis(1);

pub(crate) fn group(id: u64, tab: Tab) -> WindowGroup {
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
pub(crate) fn layout_tree() -> Layout {
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
pub(crate) fn session_on(width: i32, height: i32) -> Session {
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
    pub(crate) cards: Vec<String>,
    /// Which card the walk reached, an index into `cards`.
    pub(crate) selected: usize,
    /// The walk crosses the front app's windows (Alt+Grave) rather than the workspace's.
    pub(crate) app_scope: bool,
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
pub(crate) struct Node {
    pub(crate) role: String,
    pub(crate) label: Option<String>,
    pub(crate) bounds: Option<[f64; 4]>,
}

/// The whole laid-out tree, in place: every window's node, as assistive technology reads it from
/// the session's own export.
pub(crate) fn laid_out(session: &Session) -> Vec<Node> {
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
pub(crate) fn page_of(session: &Session) -> Vec<(usize, Rect)> {
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
pub(crate) fn until_drawn(session: &mut Session) {
    until(session, "the panel draws the cards it shows", |session| {
        let Some(panel) = panel(session) else {
            return false;
        };
        page_of(session).iter().any(|(at, _)| *at == panel.selected)
    });
}

/// Where the panel's cards stand, once the shell has drawn what it shows now.
pub(crate) fn drawn_cards(session: &mut Session) -> Vec<(usize, Rect)> {
    until_drawn(session);
    page_of(session)
}

/// Run a couple of frames after the panel is drawn, so a capture holds the frame the shell drew
/// rather than the one standing before it.
pub(crate) fn settle(session: &mut Session) {
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

/// How far the frame's pixels along a card's top edge stand from `to` : the least of the rows the
/// edge's ring covers, at three places across it, as summed channel distance.
pub(crate) fn nearest_edge_pixel(
    frame: &tilo_compositor::Screenshot,
    card: Rect,
    to: [u8; 3],
) -> u32 {
    let distance = |pixel: [u8; 4]| -> u32 {
        [0, 1, 2]
            .iter()
            .map(|at| u32::from(pixel[*at].abs_diff(to[*at])))
            .sum()
    };
    let ys = (-2..=0).map(|offset| pixel_of(card.y0) + offset);
    let xs = [0.25, 0.5, 0.75].map(|share| pixel_of(card.x0 + card.width() * share));
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
fn pixel_of(value: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a place on a display, rounded to its pixel"
    )]
    let pixel = value.round() as i32;
    pixel
}
