//! The desktop's control hints on the real chrome: what the person's record says they have not
//! learned shows over the desktop once the moment calls for it, and stays away when they turned
//! hints off.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::control_hints::{HINTS, HintsFeed};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    DesktopTree, FloatCorner, FloatPlace, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::control_hints::Learned;
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
fn desk_tree(windows: u64) -> DesktopTree {
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
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
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

/// The chrome over a desk with `windows` windows, lent `feed`.
fn chrome_with_hints(windows: u64, feed: &HintsFeed) -> FrameHarness {
    chrome_over(&desk_tree(windows), feed).0
}

/// The chrome over `tree`, lent `feed`, and the desk itself.
fn chrome_over(tree: &DesktopTree, feed: &HintsFeed) -> (FrameHarness, DesktopHandle) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_hints_feed(feed.clone());
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
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
    chrome.draw_child(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
}

/// What a case leaves the feed as, before a frame.
type Arrange = fn(&HintsFeed);

/// The record of someone who learned every hint but `unlearned`.
fn record_learning_all_but(unlearned: &str) -> Learned {
    let mut learned = Learned::default();
    for spec in HINTS.iter().filter(|spec| spec.id != unlearned) {
        learned.learn(spec.id);
    }
    learned
}

#[test]
fn a_hint_the_person_has_not_learned_shows_over_the_desktop() {
    let feed = HintsFeed::default();
    feed.set_record(record_learning_all_but("resize-window"));
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

#[test]
fn nothing_is_taught_while_the_person_may_not_learn() {
    // Each case leaves the feed as a host would before a frame.
    let cases: [(&str, Arrange); 3] = [
        ("every hint the person learned", |feed| {
            feed.set_record(record_learning_all_but(""));
            feed.set_context(None, true, Vec::new());
        }),
        ("hints turned off in the person's settings", |feed| {
            let mut learned = record_learning_all_but("resize-window");
            learned.enabled = false;
            feed.set_record(learned);
            feed.set_context(None, true, Vec::new());
        }),
        ("following someone", |feed| {
            feed.set_record(record_learning_all_but("resize-window"));
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

/// Run `visit` on the chrome the frame mounts.
fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// A strip of `windows` windows with the window `group` lifted as a card over the desk, at `dx`
/// from the left of the work area.
fn desk_with_card(windows: u64, group: u64, dx: i32) -> DesktopTree {
    let mut tree = desk_tree(windows);
    tree.workspaces[0].arrangement.set_float(
        group,
        Some(FloatPlace::new(FloatCorner::TopLeft, dx, 160, 360, 520)),
    );
    tree
}

/// The record of someone who has learned every hint but the one that teaches resizing.
fn learning_resize_only() -> Learned {
    let mut learned = Learned::default();
    for spec in HINTS.iter().filter(|spec| spec.id != "resize-window") {
        learned.learn(spec.id);
    }
    learned
}

/// A hint is taught beside the strip's own windows, and points at a handle that is there to take:
/// a card standing over the desk is another surface, so no hint is anchored to it, and none is
/// drawn over its content.
#[test]
fn no_hint_points_at_a_card_nor_covers_one() {
    // The card holds the person's attention: a card's header does not take it as a strip window's
    // bar does, and its edge resizes nothing.
    let feed = HintsFeed::default();
    feed.set_record(learning_resize_only());
    feed.set_context(None, true, Vec::new());
    let mut tree = desk_with_card(3, 5, 400);
    tree.active_group.insert(StripId(2), 5);
    let (mut frame, handle) = chrome_over(&tree, &feed);
    assert_eq!(
        handle.hint_places().windows,
        2,
        "the card is no strip window"
    );
    assert_eq!(handle.hint_places().focused_edge, None, "a card's edge");
    with_chrome(&mut frame, |chrome| {
        assert!(
            !chrome.hints().is_showing(),
            "nothing points at a card, nor at a window a card covers"
        );
    });

    // The card stands on the desk away from the handle: the hint is taught beside the handle and
    // keeps off the card's content.
    let feed = HintsFeed::default();
    feed.set_record(learning_resize_only());
    feed.set_context(None, true, Vec::new());
    let (mut frame, handle) = chrome_over(&desk_with_card(3, 5, 0), &feed);
    let cards = handle.hint_places().over;
    assert_eq!(cards.len(), 1, "the card over the desk: {cards:?}");
    with_chrome(&mut frame, |chrome| {
        let painted = chrome
            .hints()
            .card_rect()
            .expect("a free desk shows the hint beside the handle");
        for card in &cards {
            assert!(
                !painted.overlaps(*card),
                "a hint {painted:?} stands over the card {card:?}"
            );
        }
    });

    // The card stands over the handle: there is nothing there to take, so nothing is taught, and
    // no card's content is covered.
    let feed = HintsFeed::default();
    feed.set_record(learning_resize_only());
    feed.set_context(None, true, Vec::new());
    let (mut frame, handle) = chrome_over(&desk_with_card(3, 5, 700), &feed);
    let cards = handle.hint_places().over;
    assert_eq!(cards.len(), 1, "the card over the desk: {cards:?}");
    let (line, _) = handle
        .hint_places()
        .focused_edge
        .expect("fixture: the handle is on screen");
    assert!(
        cards[0].overlaps(Rect::from_points(line.p0, line.p1)),
        "fixture: the card covers the handle: {}",
        cards[0]
    );
    with_chrome(&mut frame, |chrome| {
        assert!(
            chrome.hints().card_rect().is_none(),
            "a handle under a card teaches nothing: {:?}",
            chrome.hints().card_rect()
        );
    });
}
