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
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::DesktopSurface;
use ui::control_hints::Learned;
use ui::headless::FrameHarness;
use ui::kurbo::Affine;
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
    let tree = desk_tree(windows);
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle)),
        TopBarChrome::from_tree(&tree),
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
    frame
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
