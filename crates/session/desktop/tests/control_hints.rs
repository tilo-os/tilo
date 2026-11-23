//! The desktop's control hints on the real chrome: what the person's record says they have not
//! learned shows over the desktop once the moment calls for it, and stays away when they turned
//! hints off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use crate::hint_app::SketchTiles;
use crate::support::with_chrome;
use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::control_hints::{ControlHints, HINTS, HintFrame, HintsFeed};
use tilo_desktop::desktop_host::{DesktopChrome, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::control_hints::Learned;
use ui::control_hints::performed::{self, Performed};
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Rect};
use ui::widget::DrawChild;

fn window(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// A strip holding `windows` windows, side by side: the move and resize hints are about a strip
/// holding two.
fn layout_tree(windows: u64) -> Layout {
    let groups: Vec<WindowGroup> = (0..windows)
        .map(|id| {
            let tab = window(20_u64.saturating_add(id), "Notes");
            WindowGroup {
                id: 3_u64.saturating_add(id),
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }
        })
        .collect();
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The chrome over a shell with `windows` windows, lent `feed`.
fn chrome_with_hints(windows: u64, feed: &HintsFeed) -> FrameHarness {
    chrome_over(&layout_tree(windows), feed).0
}

/// The chrome over `tree`, lent `feed`, and the shell itself.
fn chrome_over(tree: &Layout, feed: &HintsFeed) -> (FrameHarness, DesktopHandle) {
    chrome_over_at(tree, feed, 1280)
}

/// [`chrome_over`] on a screen `width` logical pixels across.
fn chrome_over_at(tree: &Layout, feed: &HintsFeed, width: u32) -> (FrameHarness, DesktopHandle) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(SketchTiles));
    // The strip reads where its windows show text and controls, as it does while a hint may show.
    handle.read_hint_ink(true);
    // The bar is shown over the desktop, as it is when the person sees it: their windows settle
    // below it, whole, as the shell tells the strip.
    handle.set_top_overlay_inset(f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX));
    // The windows settle below the bar at once, as they do when the person asks for less motion.
    let mut shell = FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (width, 800),
        ui::Theme::dark().with_reduce_motion(true),
    );
    shell.relayout();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_hints_feed(feed.clone());
    // The windows settle below the bar at once, as they do when the person asks for less motion:
    // the slide down to it is motion, not geometry, and a hint is placed against the geometry.
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (width, 800),
        ui::Theme::dark().with_reduce_motion(true),
    );
    frame.relayout();
    draw_chrome(&mut frame);
    (frame, handle)
}

/// Draw one frame of the chrome, as the session's chrome layer does.
fn draw_chrome(frame: &mut FrameHarness) {
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::default();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = ui::vello::Scene::new();
    let mut chrome = frame.root_mut::<DesktopChrome>();
    chrome.draw_child(&mut scene, Affine::IDENTITY, &mut text, &theme,&presence.draw_env());
}

/// What a case leaves the feed as, before a frame.
type Arrange = fn(&HintsFeed);

/// What someone learned: every hint but `unlearned`.
fn learning_all_but(unlearned: &str) -> Learned {
    let mut learned = Learned::default();
    for spec in HINTS.iter().filter(|spec| spec.id != unlearned) {
        learned.learn(spec.id);
    }
    learned
}

#[test]
fn a_hint_the_person_has_not_learned_shows_over_the_desktop() {
    let feed = HintsFeed::default();
    feed.set_record(learning_all_but("resize-window"));
    feed.set_context(None, true, Vec::new());
    let mut frame = chrome_with_hints(2, &feed);
    with_chrome(&mut frame, |chrome| {
        assert!(
            chrome.hints().is_showing(),
            "resizing a window is taught once a strip holds two"
        );
        assert!(
            chrome.hints().painted_rect().is_some(),
            "the hint was drawn over the desktop"
        );
    });
}

/// The overview covers the shell: a hint pointing at a window's edge or bar goes with the windows,
/// and none stands over the overview.
#[test]
fn a_hint_leaves_when_the_overview_covers_the_shell() {
    let feed = HintsFeed::default();
    feed.set_record(learning_all_but("resize-window"));
    feed.set_context(None, true, Vec::new());
    let mut frame = chrome_with_hints(2, &feed);
    assert!(with_chrome(&mut frame, |chrome| chrome
        .hints()
        .is_showing()));
    with_chrome(&mut frame, DesktopChrome::toggle_overview);
    frame.relayout();
    draw_chrome(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert!(chrome.overview_open());
        assert!(
            !chrome.hints().is_showing(),
            "the resize hint leaves with the windows the overview covers"
        );
    });
}

#[test]
fn nothing_is_taught_while_the_person_may_not_learn() {
    // Each case leaves the feed as a host would before a frame.
    let cases: [(&str, Arrange); 3] = [
        ("every hint the person learned", |feed| {
            feed.set_record(learning_all_but(""));
            feed.set_context(None, true, Vec::new());
        }),
        ("hints turned off in the person's settings", |feed| {
            let mut learned = learning_all_but("resize-window");
            learned.enabled = false;
            feed.set_record(learned);
            feed.set_context(None, true, Vec::new());
        }),
        ("following someone", |feed| {
            feed.set_record(learning_all_but("resize-window"));
            feed.set_context(Some(ui::control_hints::Quiet::Following), true, Vec::new());
        }),
    ];
    for (why, arrange) in cases {
        let feed = HintsFeed::default();
        arrange(&feed);
        let mut frame = chrome_with_hints(2, &feed);
        with_chrome(&mut frame, |chrome| {
            assert!(!chrome.hints().is_showing(), "{why}: no hint begins");
        });
    }
}

/// A shell whose bar is busy: a long workspace name on its left and many people on its right.
fn busy_shell() -> Layout {
    let mut tree = layout_tree(2);
    tree.workspaces[0].name = "Customer support and escalations for the whole team".into();
    for person in 0..6 {
        tree.workspaces[0].member_here(tilo_desktop_model::Member::named(
            collab_types::MemberId::person(format!("person-{person}")),
            format!("Person {person}"),
        ));
    }
    tree
}

/// A shell of `windows` windows with the first of them maximized.
fn maximised(windows: u64) -> Layout {
    let mut tree = layout_tree(windows);
    if let Some(group) = tree.workspaces[0].strips[0].window_groups.first_mut() {
        group.maximized = true;
    }
    tree
}

/// The shell over `tree` on a screen `width` across, with the bar's band over its top edge, and the
/// strip's row as it lays out: every window stands clear of the bar, the way the desktop stands
/// them while its bar shows.
fn shell_below_the_bar(tree: &Layout, width: u32) -> (FrameHarness, ui::widget::HintPlaces) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(SketchTiles));
    handle.read_hint_ink(true);
    // Windows come to rest below the bar at once, as they do when the person asks for less motion:
    // the slide down to it is motion, not geometry, and a hint is placed against the geometry.
    let mut shell = FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (width, 800),
        ui::Theme::dark().with_reduce_motion(true),
    );
    handle.set_top_overlay_inset(f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX));
    for _ in 0..2 {
        shell.relayout();
        draw_shell(&mut shell);
    }
    let row = handle.hint_places();
    (shell, row)
}

/// Draw one frame of the shell, as the chrome draws it under its bar.
fn draw_shell(shell: &mut FrameHarness) {
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::default();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = ui::vello::Scene::new();
    let mut surface = shell.root_mut::<DesktopSurface>();
    surface.draw_child(&mut scene, Affine::IDENTITY, &mut text, &theme,&presence.draw_env());
}

/// The pill the hint `unlearned` paints over `tree`, as the chrome drives it, with the strip's own
/// places: the real geometry the hint is placed against, and the desktop under the bar it may be
/// placed in. `cues` are what the person did just before it, for a hint whose moment is one of
/// those; they are noted here, after the shell's own frames have drawn, since a frame of the chrome
/// reads and keeps what was noted before it.
fn painted_hint(
    tree: &Layout,
    unlearned: &str,
    cues: &[Performed],
    wait: ui::time::Duration,
    width: u32,
) -> (Option<Rect>, ui::widget::HintPlaces, Rect) {
    let (_, row) = shell_below_the_bar(tree, width);
    let bar = f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX);
    let viewport = row.viewport;
    let area = Rect::new(viewport.x0, viewport.y0.max(bar), viewport.x1, viewport.y1);
    let mut hints = ControlHints::new(learning_all_but(unlearned));
    for cue in cues {
        performed::note(*cue);
    }
    let frame = HintFrame {
        row: row.clone(),
        area,
        search_keys: vec!["Ctrl".to_owned(), "K".to_owned()],
        avatar: None,
        colour: None,
        quiet: None,
        presented: true,
        touch: false,
    };
    let start = ui::time::now();
    hints.update(&frame, start);
    hints.update(&frame, start + wait);
    let mut scene = ui::vello::Scene::new();
    let mut text = ui::TextRenderer::default();
    hints.draw(
        &frame,
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &ui::Theme::dark(),
        start + wait,
    );
    (hints.card_rect(), row, area)
}

/// The tallest a hint's pill may be: a small chip a line tall, nothing like the old card with its
/// picture.
const PILL_HEIGHT_MAX: f64 = 34.0;

/// Every place the strip says its windows show text and controls.
fn ink_of(row: &ui::widget::HintPlaces) -> Vec<Rect> {
    row.ink
        .clone()
        .expect("fixture: the strip read its windows' text and controls")
}

/// Whether `card` covers none of what `row`'s windows show, and none of their content either: a
/// hint stands outside every window or on a window's bar ground, never over the body.
fn covers_nothing(card: Rect, row: &ui::widget::HintPlaces) {
    for ink in ink_of(row) {
        assert!(!card.overlaps(ink), "the hint {card:?} covers {ink:?}");
    }
    for window in &row.windows_on_strip {
        let bar = row
            .bars
            .iter()
            .find(|bar| bar.x0 == window.x0 && bar.x1 == window.x1)
            .copied()
            .unwrap_or(*window);
        let body = Rect::new(window.x0, bar.y1.max(window.y0), window.x1, window.y1);
        if !body.is_zero_area() {
            assert!(
                !card.overlaps(body),
                "the hint {card:?} stands over {body:?}"
            );
        }
    }
}

/// Whether `card` stands on a window's bar ground.
fn on_a_bar(card: Rect, row: &ui::widget::HintPlaces) -> bool {
    row.bars.iter().any(|bar| bar.contains_rect(card))
}

/// Every hint is a small pill over the desktop, under the bar and never in it, at its gesture,
/// and covering none of the windows' text, controls or content: search comes down just under the
/// bar, the view's hint sits at the edge the windows hide past, and the move and resize hints
/// stand on the bar beside the stretch or edge they teach.
#[test]
fn every_hint_is_a_pill_over_the_desktop_at_its_gesture() {
    for tree in [layout_tree(0), layout_tree(2), maximised(3), busy_shell()] {
        let (painted, row, area) = painted_hint(
            &tree,
            "open-from-search",
            &[],
            ui::time::Duration::from_secs(18),
            1280,
        );
        let card = painted.expect("the search hint shows");
        assert!(card.y0 >= area.y0, "under the bar, not in it: {card:?}");
        assert!(
            card.y1 <= area.center().y,
            "in the top half of the desktop: {card:?}"
        );
        assert!(card.height() <= PILL_HEIGHT_MAX, "a small pill: {card:?}");
        covers_nothing(card, &row);
    }
    let (painted, _, area) = painted_hint(
        &layout_tree(0),
        "open-from-search",
        &[],
        ui::time::Duration::from_secs(18),
        1280,
    );
    let card = painted.expect("the search hint shows");
    assert!(
        (card.center().x - area.center().x).abs() < 1.0,
        "on an empty desktop, at its middle: {card:?} in {area:?}"
    );
    let (painted, row, area) = painted_hint(
        &layout_tree(4),
        "pan-view",
        &[],
        ui::time::Duration::from_secs(60),
        1280,
    );
    assert!(row.beyond.1, "fixture: windows lie past the trailing edge");
    let card = painted.expect("the pan hint shows");
    assert!(
        area.x1 - card.x1 < 40.0,
        "at the edge the windows hide past: {card:?} in {area:?}"
    );
    assert!(card.height() <= PILL_HEIGHT_MAX, "a small pill: {card:?}");
    covers_nothing(card, &row);
    let (painted, row, _) = painted_hint(
        &layout_tree(2),
        "resize-window",
        &[],
        ui::time::Duration::ZERO,
        1280,
    );
    let card = painted.expect("the resize hint shows");
    let (line, _) = row.focused_edge.expect("fixture: an edge to take");
    assert!(
        on_a_bar(card, &row)
            || (card.x0 > line.p0.x && card.x0 - line.p0.x < 24.0)
            || (card.x1 < line.p0.x && line.p0.x - card.x1 < 24.0),
        "beside the edge it teaches, or on the bar above it: {card:?} by {line:?}"
    );
    covers_nothing(card, &row);
    let (painted, row, _) = painted_hint(
        &layout_tree(3),
        "move-window",
        &[Performed::HoveredWindowBar, Performed::OpenedWindow],
        ui::time::Duration::ZERO,
        1280,
    );
    let card = painted.expect("the move hint shows");
    let bar = row.focused_bar.expect("fixture: a bar to take");
    let window_bar = row
        .bars
        .iter()
        .find(|b| b.contains_rect(bar))
        .copied()
        .expect("fixture: the stretch sits on a bar");
    assert!(
        window_bar.contains_rect(card),
        "on the bar it teaches, beside the stretch that takes it: {card:?} on {window_bar:?}"
    );
    assert!(
        !card.overlaps(bar),
        "beside the stretch, never on it: {card:?} against {bar:?}"
    );
    covers_nothing(card, &row);
}

/// Notes and Chat side by side filling the desktop, Chat in front with its trailing edge at the
/// screen's: nothing past that edge. The resize hint's card stands beside it on Chat's empty
/// ground, never over the words of Chat's empty state nor any other text or control, however
/// wide the screen.
#[test]
fn the_resize_hint_beside_the_screens_edge_covers_no_text_or_control() {
    for width in [1280, 1400, 1920] {
        let mut tree = layout_tree(0);
        let half = |id: u64, title: &str| WindowGroup {
            id,
            active_tab: id.saturating_add(20),
            tabs: vec![window(id.saturating_add(20), title)],
            width: WindowWidth::Fraction(500),
            maximized: false,
            focus_history: Vec::new(),
        };
        tree.workspaces[0].strips[0].window_groups = vec![half(3, "Notes"), half(4, "Chat")];
        tree.active_group.insert(StripId(2), 4);
        let (painted, row, _) =
            painted_hint(&tree, "resize-window", &[], ui::time::Duration::ZERO, width);
        let (line, _) = row.focused_edge.expect("fixture: Chat's edge");
        assert!(
            row.windows_on_strip.len() == 2,
            "fixture: two windows {:?}",
            row.windows_on_strip
        );
        let card = painted.expect("Chat has its bar's ground");
        covers_nothing(card, &row);
        assert!(
            on_a_bar(card, &row)
                || (card.x0 > line.p0.x && card.x0 - line.p0.x < 24.0)
                || (card.x1 < line.p0.x && line.p0.x - card.x1 < 24.0),
            "{width}: beside the edge it teaches, or on the bar above it: {card:?} by {line:?}"
        );
    }
}
